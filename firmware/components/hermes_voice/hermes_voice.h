// SPDX-License-Identifier: GPL-3.0-only
#pragma once

#ifdef USE_ESP32

#include "esphome/components/micro_wake_word/micro_wake_word.h"
#include "esphome/components/microphone/microphone_source.h"
#include "esphome/components/ring_buffer/ring_buffer.h"
#include "esphome/components/speaker/speaker.h"
#include "esphome/core/automation.h"
#include "esphome/core/component.h"
#include "esphome/core/static_task.h"

#include <esp_event.h>
#include <esp_websocket_client.h>
#include <freertos/FreeRTOS.h>
#include <freertos/queue.h>
#include <freertos/semphr.h>

#include <array>
#include <atomic>
#include <cstddef>
#include <cstdint>
#include <memory>
#include <string>
#include <vector>

namespace esphome::hermes_voice {

class PsramRingBuffer : public ring_buffer::RingBuffer {
 public:
  static std::unique_ptr<PsramRingBuffer> create(size_t length);
};

enum class State : uint8_t {
  IDLE = 0,
  WAITING_FOR_WAKE_WORD_STOP,
  STARTING_MICROPHONE,
  LISTENING,
  PROCESSING,
  PLAYING,
  STOPPING_MICROPHONE,
  ERROR,
};

enum class InboundEventType : uint8_t {
  READY = 0,
  CONVERSATION_RESET_DONE,
  TURN_READY,
  INPUT_ACK,
  TRANSCRIPT_FINAL,
  RESPONSE_START,
  TTS_START,
  TTS_END,
  TURN_DONE,
  ERROR,
};

struct InboundEvent {
  InboundEventType type{InboundEventType::READY};
  uint64_t turn_id{0};
  uint64_t request_id{0};
  uint32_t sequence{UINT32_MAX};
  uint32_t value{0};
  bool fatal{false};
  char conversation_id[129]{};
  char code[48]{};
  char message[160]{};
};

struct OutboundControl {
  uint16_t length{0};
  char data[384]{};
};

struct OutputFrameCredit {
  uint32_t sequence{0};
  uint32_t remaining_bytes{0};
};

class HermesVoice : public Component {
 public:
  void setup() override;
  void loop() override;
  void dump_config() override;
  void on_shutdown() override;
  float get_setup_priority() const override;

  void start();
  void stop();
  void new_conversation();
  // The base package calls these only after BLE provisioning has fully stopped
  // and before it is re-enabled. They keep the persistent gateway socket out
  // of the BLE/Improv recovery window without changing conversation state.
  void resume_transport();
  void suspend_transport();
  bool is_running() const { return this->state_ != State::IDLE; }
  bool is_configured() const { return this->configured_; }
  bool is_ready() const {
    return this->configured_ && this->websocket_connected_.load(std::memory_order_acquire) &&
           this->protocol_ready_.load(std::memory_order_acquire) &&
           !this->conversation_reset_pending_.load(std::memory_order_acquire);
  }
  const std::string &get_device_id() const { return this->device_id_; }

  void set_microphone_source(microphone::MicrophoneSource *source) { this->microphone_source_ = source; }
  void set_speaker(speaker::Speaker *speaker) { this->speaker_ = speaker; }
  void set_micro_wake_word(micro_wake_word::MicroWakeWord *mww) { this->micro_wake_word_ = mww; }
  void set_gateway_url(const std::string &value) { this->gateway_url_ = value; }
  void set_auth_token(const std::string &value) { this->auth_token_ = value; }
  void set_device_id(const std::string &value) { this->device_id_ = value; }
  void set_silence_threshold(uint16_t value) { this->silence_threshold_ = value; }
  void set_silence_duration_ms(uint32_t value) { this->silence_duration_ms_ = value; }
  void set_speech_timeout_ms(uint32_t value) { this->speech_timeout_ms_ = value; }
  void set_max_recording_duration_ms(uint32_t value) { this->max_recording_duration_ms_ = value; }
  void set_min_recording_duration_ms(uint32_t value) { this->min_recording_duration_ms_ = value; }
  void set_request_timeout_ms(uint32_t value) { this->request_timeout_ms_ = value; }
  void set_output_sample_rate(uint32_t value) { this->output_sample_rate_ = value; }
  void set_task_stack_in_psram(bool value) { this->task_stack_in_psram_ = value; }

  Trigger<> *get_listening_trigger() { return &this->listening_trigger_; }
  Trigger<> *get_speech_start_trigger() { return &this->speech_start_trigger_; }
  Trigger<> *get_thinking_trigger() { return &this->thinking_trigger_; }
  Trigger<> *get_replying_trigger() { return &this->replying_trigger_; }
  Trigger<> *get_idle_trigger() { return &this->idle_trigger_; }
  Trigger<std::string, std::string> *get_error_trigger() { return &this->error_trigger_; }

  static constexpr uint8_t PROTOCOL_VERSION = 2;
  static constexpr uint8_t INPUT_AUDIO_KIND = 1;
  static constexpr uint8_t OUTPUT_AUDIO_KIND = 2;
  static constexpr size_t BINARY_HEADER_SIZE = 20;
  static constexpr uint32_t INPUT_SAMPLE_RATE = 16000;
  static constexpr uint8_t INPUT_CHANNELS = 1;
  static constexpr uint8_t INPUT_BITS_PER_SAMPLE = 16;
  static constexpr size_t AUDIO_FRAME_BYTES = 2048;  // 64 ms PCM16/16 kHz mono
  static constexpr uint32_t AUDIO_FRAME_DURATION_MS = 64;
  // Holds the complete configured 30 s capture while per-turn STT performs
  // bounded recovery/connect/session startup. Audio still drains in realtime
  // as soon as turn.ready arrives; this is a cold-start safety bound in PSRAM.
  static constexpr size_t INPUT_RING_BYTES = 1024 * 1024;
  static constexpr size_t OUTPUT_RING_BYTES = 128 * 1024;

 protected:
  static constexpr uint32_t SENDER_TASK_STACK_SIZE = 4096;
  static constexpr UBaseType_t SENDER_TASK_PRIORITY = 2;
  static constexpr size_t MAX_INBOUND_MESSAGE_BYTES = 8192 + BINARY_HEADER_SIZE;
  static constexpr size_t SPEAKER_PENDING_BYTES = 4096;
  static constexpr uint32_t COMPONENT_TRANSITION_TIMEOUT_MS = 5000;
  static constexpr uint32_t PLAYBACK_DRAIN_TIMEOUT_MS = 30000;
  static constexpr uint32_t BARGE_IN_SPEECH_MS = 160;
  static constexpr uint32_t INPUT_WINDOW_FRAMES = 32;
  static constexpr uint32_t OUTPUT_WINDOW_FRAMES = 32;
  static constexpr uint32_t ACK_EVERY_FRAMES = 4;
  static constexpr uint32_t RECONNECT_INITIAL_DELAY_MS = 1000;
  static constexpr uint32_t RECONNECT_MAX_DELAY_MS = 300000;

  void set_state_(State state);
  void begin_capture_();
  void commit_turn_();
  void begin_barge_in_();
  void finish_turn_();
  void return_to_idle_();
  void set_error_(const char *code, const char *message);
  void cancel_active_turn_(const char *reason);
  void reset_turn_state_(bool preserve_input);
  void handle_microphone_data_(const std::vector<uint8_t> &data);
  void process_inbound_events_();
  void service_playback_();
  void clear_output_audio_();
  void request_realtime_wifi_();
  void release_realtime_wifi_();

  bool initialize_websocket_();
  bool start_websocket_task_();
  bool restart_websocket_();
  void schedule_start_retry_(uint32_t now);
  void service_start_retry_(uint32_t now);
  static void websocket_event_handler_(void *handler_args, esp_event_base_t base, int32_t event_id, void *event_data);
  void handle_websocket_event_(int32_t event_id, esp_websocket_event_data_t *event);
  void handle_websocket_data_(const esp_websocket_event_data_t *event);
  void reset_inbound_message_();
  void process_inbound_message_(uint8_t opcode, const uint8_t *data, size_t size);
  void process_control_message_(const uint8_t *data, size_t size);
  void process_output_audio_(const uint8_t *data, size_t size);

  bool start_sender_task_();
  static void sender_task_entry_(void *parameter);
  void run_sender_task_();
  bool send_text_(const char *data, size_t size);
  bool send_binary_(const uint8_t *data, size_t size);
  bool enqueue_control_(const char *format, ...) __attribute__((format(printf, 2, 3)));
  bool enqueue_inbound_(const InboundEvent &event);
  bool acknowledge_output_through_(uint64_t turn_id, uint32_t sequence);

  uint64_t generate_turn_id_() const;
  uint64_t get_active_turn_id_();
  void set_active_turn_id_(uint64_t value);
  uint64_t get_conversation_reset_request_id_();
  void set_conversation_reset_request_id_(uint64_t value);

  microphone::MicrophoneSource *microphone_source_{nullptr};
  speaker::Speaker *speaker_{nullptr};
  micro_wake_word::MicroWakeWord *micro_wake_word_{nullptr};

  std::string gateway_url_;
  std::string auth_token_;
  std::string device_id_;
  std::string handshake_headers_;
  std::string conversation_id_;

  uint16_t silence_threshold_{250};
  uint32_t silence_duration_ms_{500};
  uint32_t speech_timeout_ms_{8000};
  uint32_t max_recording_duration_ms_{30000};
  uint32_t min_recording_duration_ms_{300};
  uint32_t request_timeout_ms_{300000};
  uint32_t output_sample_rate_{16000};
  bool task_stack_in_psram_{false};
  bool configured_{false};

  State state_{State::IDLE};
  uint32_t state_started_ms_{0};
  uint32_t capture_started_ms_{0};
  uint32_t turn_started_ms_{0};
  uint32_t playback_ended_ms_{0};
  bool speech_start_triggered_{false};
  bool replying_triggered_{false};
  bool speaker_finish_requested_{false};
  bool realtime_wifi_requested_{false};
  bool remote_turn_done_{false};
  bool response_started_{false};
  bool tts_started_{false};
  bool tts_ended_{false};

  std::unique_ptr<PsramRingBuffer> input_buffer_;
  std::unique_ptr<PsramRingBuffer> output_buffer_;
  std::array<uint8_t, SPEAKER_PENDING_BYTES> speaker_pending_{};
  size_t speaker_pending_size_{0};
  size_t speaker_pending_offset_{0};

  QueueHandle_t outbound_queue_{nullptr};
  QueueHandle_t inbound_queue_{nullptr};
  QueueHandle_t output_credit_queue_{nullptr};
  SemaphoreHandle_t output_buffer_mutex_{nullptr};
  StaticTask sender_task_;
  esp_websocket_client_handle_t websocket_{nullptr};

  uint8_t *inbound_message_buffer_{nullptr};
  size_t inbound_message_size_{0};
  uint8_t inbound_message_opcode_{0};
  bool inbound_message_active_{false};
  uint64_t output_validation_turn_id_{0};
  uint32_t expected_output_sequence_{0};
  uint32_t expected_output_sample_{0};
  OutputFrameCredit output_credit_current_{};
  bool output_credit_current_valid_{false};

  portMUX_TYPE turn_id_mux_ = portMUX_INITIALIZER_UNLOCKED;
  uint64_t active_turn_id_{0};
  portMUX_TYPE conversation_reset_request_id_mux_ = portMUX_INITIALIZER_UNLOCKED;
  uint64_t conversation_reset_request_id_{0};

  std::atomic<bool> shutting_down_{false};
  std::atomic<bool> websocket_connected_{false};
  std::atomic<bool> protocol_ready_{false};
  std::atomic<bool> hello_pending_{false};
  std::atomic<bool> connection_lost_{false};
  std::atomic<bool> transport_allowed_{false};
  std::atomic<bool> transport_started_{false};
  std::atomic<bool> start_retry_requested_{false};
  std::atomic<uint32_t> reconnect_base_delay_ms_{RECONNECT_INITIAL_DELAY_MS};
  std::atomic<bool> send_failed_{false};
  std::atomic<bool> protocol_error_{false};
  std::atomic<bool> conversation_reset_pending_{false};
  std::atomic<bool> conversation_reset_send_requested_{false};

  bool start_retry_pending_{false};
  uint32_t start_retry_at_ms_{0};

  std::atomic<bool> turn_active_{false};
  std::atomic<bool> turn_start_requested_{false};
  std::atomic<bool> turn_ready_{false};
  std::atomic<bool> capturing_{false};
  std::atomic<uint32_t> microphone_callbacks_inflight_{0};
  std::atomic<bool> commit_requested_{false};
  std::atomic<bool> commit_sent_{false};
  std::atomic<bool> barge_monitoring_{false};
  std::atomic<bool> barge_requested_{false};
  std::atomic<uint32_t> barge_voice_started_ms_{0};

  std::atomic<bool> speech_seen_{false};
  std::atomic<uint32_t> last_voice_ms_{0};
  std::atomic<uint32_t> captured_samples_{0};
  std::atomic<bool> input_buffer_overflow_{false};
  std::atomic<bool> output_buffer_overflow_{false};
  std::atomic<uint32_t> sent_input_count_{0};
  std::atomic<uint32_t> acked_input_count_{0};
  std::atomic<uint32_t> accepted_output_count_{0};
  std::atomic<uint32_t> consumed_output_count_{0};
  std::atomic<uint32_t> acknowledged_output_count_{0};
  std::atomic<uint32_t> last_output_sequence_{UINT32_MAX};

  Trigger<> listening_trigger_;
  Trigger<> speech_start_trigger_;
  Trigger<> thinking_trigger_;
  Trigger<> replying_trigger_;
  Trigger<> idle_trigger_;
  Trigger<std::string, std::string> error_trigger_;
};

template<typename... Ts> class StartAction : public Action<Ts...>, public Parented<HermesVoice> {
 public:
  void play(const Ts &...x) override { this->parent_->start(); }
};

template<typename... Ts> class StopAction : public Action<Ts...>, public Parented<HermesVoice> {
 public:
  void play(const Ts &...x) override { this->parent_->stop(); }
};

template<typename... Ts> class NewConversationAction : public Action<Ts...>, public Parented<HermesVoice> {
 public:
  void play(const Ts &...x) override { this->parent_->new_conversation(); }
};

template<typename... Ts> class IsRunningCondition : public Condition<Ts...>, public Parented<HermesVoice> {
 public:
  bool check(const Ts &...x) override { return this->parent_->is_running(); }
};

}  // namespace esphome::hermes_voice

#endif  // USE_ESP32
