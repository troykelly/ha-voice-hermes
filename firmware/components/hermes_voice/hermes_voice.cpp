// SPDX-License-Identifier: GPL-3.0-only
#include "hermes_voice.h"

#ifdef USE_ESP32

#include "esphome/components/audio/audio.h"
#include "esphome/components/json/json_util.h"
#include "esphome/components/network/util.h"
#ifdef USE_WIFI
#include "esphome/components/wifi/wifi_component.h"
#endif
#include "esphome/core/application.h"
#include "esphome/core/defines.h"
#include "esphome/core/helpers.h"
#include "esphome/core/log.h"

#include <algorithm>
#include <cstdarg>
#include <cstdio>
#include <cstring>
#include <inttypes.h>

#include <esp_crt_bundle.h>
#include <esp_system.h>

namespace esphome::hermes_voice {

static const char *const TAG = "hermes_voice";
static constexpr char FIRMWARE_VERSION[] = "ha-voice-hermes/" ESPHOME_PROJECT_VERSION;
static constexpr char REALTIME_SUBPROTOCOL[] = "hermes-voice.realtime.v2";
static constexpr char PCM_FORMAT[] = "pcm_s16le_16000_mono";
static constexpr uint8_t WS_OPCODE_CONTINUATION = 0x00;
static constexpr uint8_t WS_OPCODE_TEXT = 0x01;
static constexpr uint8_t WS_OPCODE_BINARY = 0x02;
static constexpr uint8_t WS_OPCODE_CLOSE = 0x08;
static constexpr uint8_t WS_OPCODE_PING = 0x09;
static constexpr uint8_t WS_OPCODE_PONG = 0x0A;

class SemaphoreGuard {
 public:
  explicit SemaphoreGuard(SemaphoreHandle_t semaphore, TickType_t ticks_to_wait = pdMS_TO_TICKS(50))
      : semaphore_(semaphore) {
    this->locked_ = semaphore != nullptr && xSemaphoreTake(semaphore, ticks_to_wait) == pdTRUE;
  }
  ~SemaphoreGuard() {
    if (this->locked_)
      xSemaphoreGive(this->semaphore_);
  }
  bool locked() const { return this->locked_; }

 protected:
  SemaphoreHandle_t semaphore_;
  bool locked_{false};
};

std::unique_ptr<PsramRingBuffer> PsramRingBuffer::create(size_t length) {
  auto buffer = std::unique_ptr<PsramRingBuffer>(new PsramRingBuffer());
  RAMAllocator<uint8_t> external_allocator(RAMAllocator<uint8_t>::ALLOC_EXTERNAL);
  buffer->size_ = length;
  buffer->storage_ = external_allocator.allocate(length);
  if (buffer->storage_ == nullptr)
    return nullptr;
  buffer->handle_ = xRingbufferCreateStatic(length, RINGBUF_TYPE_BYTEBUF, buffer->storage_, &buffer->structure_);
  if (buffer->handle_ == nullptr) {
    external_allocator.deallocate(buffer->storage_, length);
    buffer->storage_ = nullptr;
    return nullptr;
  }
  return buffer;
}

static const char *state_to_string(State state) {
  switch (state) {
    case State::IDLE:
      return "IDLE";
    case State::WAITING_FOR_WAKE_WORD_STOP:
      return "WAITING_FOR_WAKE_WORD_STOP";
    case State::STARTING_MICROPHONE:
      return "STARTING_MICROPHONE";
    case State::LISTENING:
      return "LISTENING";
    case State::PROCESSING:
      return "PROCESSING";
    case State::PLAYING:
      return "PLAYING";
    case State::STOPPING_MICROPHONE:
      return "STOPPING_MICROPHONE";
    case State::ERROR:
      return "ERROR";
  }
  return "UNKNOWN";
}

static void write_u32_be(uint8_t *output, uint32_t value) {
  output[0] = static_cast<uint8_t>(value >> 24);
  output[1] = static_cast<uint8_t>(value >> 16);
  output[2] = static_cast<uint8_t>(value >> 8);
  output[3] = static_cast<uint8_t>(value);
}

static void write_u64_be(uint8_t *output, uint64_t value) {
  write_u32_be(output, static_cast<uint32_t>(value >> 32));
  write_u32_be(output + 4, static_cast<uint32_t>(value));
}

static uint32_t read_u32_be(const uint8_t *input) {
  return (static_cast<uint32_t>(input[0]) << 24) | (static_cast<uint32_t>(input[1]) << 16) |
         (static_cast<uint32_t>(input[2]) << 8) | static_cast<uint32_t>(input[3]);
}

static uint64_t read_u64_be(const uint8_t *input) {
  return (static_cast<uint64_t>(read_u32_be(input)) << 32) | read_u32_be(input + 4);
}

static int hex_nibble(char value) {
  if (value >= '0' && value <= '9')
    return value - '0';
  if (value >= 'a' && value <= 'f')
    return value - 'a' + 10;
  if (value >= 'A' && value <= 'F')
    return value - 'A' + 10;
  return -1;
}

static bool parse_turn_id(const char *value, uint64_t &turn_id) {
  if (value == nullptr || strlen(value) != 16)
    return false;
  uint64_t parsed = 0;
  for (size_t i = 0; i < 16; i++) {
    const int nibble = hex_nibble(value[i]);
    if (nibble < 0)
      return false;
    parsed = (parsed << 4) | static_cast<uint64_t>(nibble);
  }
  if (parsed == 0)
    return false;
  turn_id = parsed;
  return true;
}

static bool parse_request_id(const char *value, uint64_t &request_id) {
  if (value == nullptr || strlen(value) != 16)
    return false;
  uint64_t parsed = 0;
  for (size_t i = 0; i < 16; i++) {
    const char character = value[i];
    const int nibble = character >= '0' && character <= '9'   ? character - '0'
                       : character >= 'a' && character <= 'f' ? character - 'a' + 10
                                                              : -1;
    if (nibble < 0)
      return false;
    parsed = (parsed << 4) | static_cast<uint64_t>(nibble);
  }
  if (parsed == 0)
    return false;
  request_id = parsed;
  return true;
}

static bool copy_conversation_id(const char *value, char *output, size_t output_size) {
  if (value == nullptr || output == nullptr || output_size == 0)
    return false;
  const size_t length = strlen(value);
  if (length == 0 || length >= output_size)
    return false;
  for (size_t i = 0; i < length; i++) {
    const auto byte = static_cast<uint8_t>(value[i]);
    if (byte < 0x21 || byte > 0x7E)
      return false;
  }
  memcpy(output, value, length + 1);
  return true;
}

float HermesVoice::get_setup_priority() const { return setup_priority::AFTER_CONNECTION; }

void HermesVoice::setup() {
  if (this->device_id_.empty()) {
    std::array<char, MAC_ADDRESS_BUFFER_SIZE> mac{};
    get_mac_address_into_buffer(mac);
    this->device_id_ = "voice-pe-";
    this->device_id_.append(mac.data());
  }

  const bool has_gateway = !this->gateway_url_.empty();
  const bool has_token = !this->auth_token_.empty();
  if (has_gateway != has_token) {
    ESP_LOGE(TAG, "Gateway URL and device token must be configured together");
    this->mark_failed();
    return;
  }
  this->configured_ = has_gateway && has_token;
  if (!this->configured_) {
    ESP_LOGI(TAG, "Hermes is not configured; ESPHome provisioning, API, OTA and media remain available");
    return;
  }

  if (this->microphone_source_ == nullptr || this->speaker_ == nullptr || this->time_source_ == nullptr) {
    ESP_LOGE(TAG, "Microphone, speaker, and a synchronized time source are required");
    this->mark_failed();
    return;
  }
  if (this->max_recording_duration_ms_ > MAX_RECORDING_DURATION_LIMIT_MS ||
      this->min_recording_duration_ms_ > this->max_recording_duration_ms_) {
    ESP_LOGE(TAG, "Recording duration exceeds the bounded realtime/STT contract");
    this->mark_failed();
    return;
  }

  const auto stream_info = this->microphone_source_->get_audio_stream_info();
  if (stream_info.get_sample_rate() != INPUT_SAMPLE_RATE || stream_info.get_channels() != INPUT_CHANNELS ||
      stream_info.get_bits_per_sample() != INPUT_BITS_PER_SAMPLE) {
    ESP_LOGE(TAG, "Microphone must expose 16 kHz, mono, 16-bit PCM");
    this->mark_failed();
    return;
  }

  this->input_buffer_ = PsramRingBuffer::create(INPUT_RING_BYTES);
  this->output_buffer_ = PsramRingBuffer::create(OUTPUT_RING_BYTES);
  RAMAllocator<uint8_t> external_allocator(RAMAllocator<uint8_t>::ALLOC_EXTERNAL);
  this->inbound_message_buffer_ = external_allocator.allocate(MAX_INBOUND_MESSAGE_BYTES);
  this->outbound_queue_ = xQueueCreate(16, sizeof(OutboundControl));
  this->inbound_queue_ = xQueueCreate(24, sizeof(InboundEvent));
  this->output_credit_queue_ = xQueueCreate(OUTPUT_WINDOW_FRAMES, sizeof(OutputFrameCredit));
  this->input_buffer_mutex_ = xSemaphoreCreateMutex();
  this->output_buffer_mutex_ = xSemaphoreCreateMutex();
  if (this->input_buffer_ == nullptr || this->output_buffer_ == nullptr || this->inbound_message_buffer_ == nullptr ||
      this->outbound_queue_ == nullptr || this->inbound_queue_ == nullptr || this->output_credit_queue_ == nullptr ||
      this->input_buffer_mutex_ == nullptr || this->output_buffer_mutex_ == nullptr) {
    ESP_LOGE(TAG, "Could not allocate bounded realtime buffers");
    this->cleanup_resources_();
    this->mark_failed();
    return;
  }

  this->speaker_->set_audio_stream_info(audio::AudioStreamInfo(16, 1, this->output_sample_rate_));
  if (!this->initialize_websocket_() || !this->start_sender_task_()) {
    ESP_LOGE(TAG, "Could not initialize the realtime WebSocket transport");
    this->cleanup_resources_();
    this->mark_failed();
    return;
  }
  this->set_microphone_callbacks_enabled_(true);
  this->microphone_source_->add_data_callback(
      [this](const std::vector<uint8_t> &data) { this->handle_microphone_data_(data); });
#ifdef USE_OTA_STATE_LISTENER
  ota::get_global_ota_callback()->add_global_state_listener(this);
#endif
  this->tls_time_valid_.store(this->time_source_->now().is_valid(), std::memory_order_release);
  // Wi-Fi may have completed its BLE grace-period automation before this
  // component's deferred setup ran. Honour that already-latched permission.
  if (this->transport_allowed_.load(std::memory_order_acquire))
    this->resume_transport();
}

void HermesVoice::dump_config() {
  ESP_LOGCONFIG(TAG, "Hermes Voice Realtime:");
  ESP_LOGCONFIG(TAG, "  Configured: %s", YESNO(this->configured_));
  ESP_LOGCONFIG(TAG, "  Device ID: %s", this->device_id_.c_str());
  if (!this->configured_)
    return;
  ESP_LOGCONFIG(TAG,
                "  Protocol: %s\n"
                "  Input ring: %u bytes\n"
                "  Output ring: %u bytes\n"
                "  Frame: %u bytes (%u ms)\n"
                "  Silence threshold: %u RMS\n"
                "  Silence duration: %" PRIu32 " ms\n"
                "  Speech timeout: %" PRIu32 " ms\n"
                "  Request timeout: %" PRIu32 " ms",
                REALTIME_SUBPROTOCOL, static_cast<unsigned>(INPUT_RING_BYTES), static_cast<unsigned>(OUTPUT_RING_BYTES),
                static_cast<unsigned>(AUDIO_FRAME_BYTES),
                static_cast<unsigned>(AUDIO_FRAME_BYTES * 1000 / (INPUT_SAMPLE_RATE * 2)), this->silence_threshold_,
                this->silence_duration_ms_, this->speech_timeout_ms_, this->request_timeout_ms_);
}

void HermesVoice::on_shutdown() {
  if (!this->configured_ || this->sender_task_.get_handle() == nullptr)
    return;
  this->shutting_down_.store(true, std::memory_order_release);
  this->transport_allowed_.store(false, std::memory_order_release);
  this->turn_active_.store(false, std::memory_order_release);
  this->capturing_.store(false, std::memory_order_release);
  this->barge_monitoring_.store(false, std::memory_order_release);
  this->commit_requested_.store(false, std::memory_order_release);
  this->set_microphone_callbacks_enabled_(false);
  if (this->microphone_source_ != nullptr)
    this->microphone_source_->stop();
  if (this->speaker_ != nullptr)
    this->speaker_->stop();
  this->notify_sender_();

  // WebSocket stop can block in ESP-IDF. The dedicated transport task owns
  // that call; final shutdown waits only for a short, watchdog-fed bound.
  const uint32_t wait_started = millis();
  while ((!this->shutdown_complete_.load(std::memory_order_acquire) ||
          !this->microphone_source_->is_stopped() ||
          this->microphone_callbacks_inflight_.load(std::memory_order_acquire) != 0) &&
         millis() - wait_started < TRANSPORT_STOP_WAIT_MS) {
    App.feed_wdt();
    delay(10);
  }
  if (!this->shutdown_complete_.load(std::memory_order_acquire) || !this->microphone_source_->is_stopped() ||
      this->microphone_callbacks_inflight_.load(std::memory_order_acquire) != 0)
    ESP_LOGW(TAG, "Realtime audio/transport did not quiesce before shutdown deadline; leaving resources for reboot");
  // Microphone callbacks are permanently registered and an already-entered
  // callback may outlive stop(). Never free its ring/mutex or sender-task
  // notification target during normal shutdown; reboot/powerdown reclaims all
  // resources. cleanup_resources_ is reserved for setup failure before the
  // callback is registered.
  this->release_realtime_wifi_();
}

#ifdef USE_OTA_STATE_LISTENER
void HermesVoice::on_ota_global_state(ota::OTAState state, float progress, uint8_t error,
                                      ota::OTAComponent *component) {
  (void) progress;
  (void) error;
  (void) component;
  if (!this->configured_)
    return;
  if (state == ota::OTA_STARTED) {
    this->prepare_for_ota_();
  } else if (state == ota::OTA_ABORT || state == ota::OTA_ERROR) {
    this->recover_from_ota_();
  }
}
#endif

void HermesVoice::set_state_(State state) {
  if (state == this->state_)
    return;
  ESP_LOGD(TAG, "State changed from %s to %s", state_to_string(this->state_), state_to_string(state));
  this->state_ = state;
  this->state_started_ms_ = millis();
}

uint64_t HermesVoice::generate_turn_id_() const {
  uint64_t turn_id = (static_cast<uint64_t>(esp_random()) << 32) | esp_random();
  return turn_id == 0 ? 1 : turn_id;
}

uint64_t HermesVoice::get_active_turn_id_() {
  portENTER_CRITICAL(&this->turn_id_mux_);
  const uint64_t value = this->active_turn_id_;
  portEXIT_CRITICAL(&this->turn_id_mux_);
  return value;
}

void HermesVoice::set_active_turn_id_(uint64_t value) {
  portENTER_CRITICAL(&this->turn_id_mux_);
  this->active_turn_id_ = value;
  portEXIT_CRITICAL(&this->turn_id_mux_);
}

uint64_t HermesVoice::get_conversation_reset_request_id_() {
  portENTER_CRITICAL(&this->conversation_reset_request_id_mux_);
  const uint64_t value = this->conversation_reset_request_id_;
  portEXIT_CRITICAL(&this->conversation_reset_request_id_mux_);
  return value;
}

void HermesVoice::set_conversation_reset_request_id_(uint64_t value) {
  portENTER_CRITICAL(&this->conversation_reset_request_id_mux_);
  this->conversation_reset_request_id_ = value;
  portEXIT_CRITICAL(&this->conversation_reset_request_id_mux_);
}

void HermesVoice::request_realtime_wifi_() {
#if defined(USE_WIFI) && defined(USE_WIFI_RUNTIME_POWER_SAVE)
  if (!this->realtime_wifi_requested_ && wifi::global_wifi_component != nullptr) {
    if (wifi::global_wifi_component->request_high_performance()) {
      this->realtime_wifi_requested_ = true;
    }
  }
#endif
}

void HermesVoice::release_realtime_wifi_() {
#if defined(USE_WIFI) && defined(USE_WIFI_RUNTIME_POWER_SAVE)
  if (this->realtime_wifi_requested_ && wifi::global_wifi_component != nullptr) {
    wifi::global_wifi_component->release_high_performance();
    this->realtime_wifi_requested_ = false;
  }
#endif
}

bool HermesVoice::reset_input_audio_() {
  if (this->input_buffer_ == nullptr || this->input_buffer_mutex_ == nullptr)
    return false;
  SemaphoreGuard lock(this->input_buffer_mutex_);
  if (!lock.locked())
    return false;
  // Increment while holding the consumer lock. A callback that latched the
  // old generation before this reset rechecks after acquiring the same lock
  // and cannot append stale PCM into the new turn.
  const uint32_t generation = this->input_generation_.fetch_add(1, std::memory_order_acq_rel) + 1;
  if (this->input_buffer_->reset() != pdPASS)
    return false;
  this->capture_generation_.store(generation, std::memory_order_release);
  this->notify_sender_();
  return true;
}

bool HermesVoice::reset_turn_state_(bool preserve_input) {
  if (!preserve_input && !this->reset_input_audio_())
    return false;
  this->clear_output_audio_();

  this->turn_ready_.store(false, std::memory_order_release);
  this->capturing_.store(false, std::memory_order_release);
  this->commit_requested_.store(false, std::memory_order_release);
  this->commit_sent_.store(false, std::memory_order_release);
  this->barge_monitoring_.store(false, std::memory_order_release);
  this->barge_requested_.store(false, std::memory_order_release);
  this->barge_voice_started_ms_.store(0, std::memory_order_release);
  this->speech_seen_.store(false, std::memory_order_release);
  this->last_voice_ms_.store(0, std::memory_order_release);
  this->input_buffer_overflow_.store(false, std::memory_order_release);
  this->output_buffer_overflow_.store(false, std::memory_order_release);
  this->sent_input_count_.store(0, std::memory_order_release);
  this->acked_input_count_.store(0, std::memory_order_release);
  this->accepted_output_count_.store(0, std::memory_order_release);
  this->consumed_output_count_.store(0, std::memory_order_release);
  this->acknowledged_output_count_.store(0, std::memory_order_release);
  this->last_output_sequence_.store(UINT32_MAX, std::memory_order_release);

  this->remote_turn_done_ = false;
  this->tts_started_ = false;
  this->tts_ended_ = false;
  this->speaker_finish_requested_ = false;
  this->replying_triggered_ = false;
  this->playback_drain_started_ms_ = 0;
  return true;
}

void HermesVoice::start() {
  if (!this->configured_) {
    ESP_LOGW(TAG, "Hermes is not configured; adopt the device and add its gateway URL and device token");
    return;
  }
  if (this->is_failed()) {
    ESP_LOGW(TAG, "Cannot start because the component failed setup");
    return;
  }
  if (this->conversation_reset_pending_.load(std::memory_order_acquire)) {
    ESP_LOGW(TAG, "Waiting for the new Hermes conversation to be acknowledged");
    return;
  }
  if ((this->state_ == State::PROCESSING || this->state_ == State::PLAYING) &&
      this->barge_monitoring_.load(std::memory_order_acquire)) {
    ESP_LOGI(TAG, "Wake word accepted as a realtime barge-in request");
    this->barge_requested_.store(true, std::memory_order_release);
    return;
  }
  if (this->state_ != State::IDLE) {
    ESP_LOGW(TAG, "A voice turn is already active");
    return;
  }
  if (!network::is_connected()) {
    this->set_error_("network_unavailable", "Wi-Fi is not connected");
    return;
  }
  if (!this->websocket_connected_.load(std::memory_order_acquire) ||
      !this->protocol_ready_.load(std::memory_order_acquire)) {
    this->set_error_("gateway_not_ready", "Realtime gateway is not ready");
    return;
  }

  this->request_realtime_wifi_();
  if (this->micro_wake_word_ != nullptr && this->micro_wake_word_->is_running())
    this->micro_wake_word_->stop();
  this->set_state_(State::WAITING_FOR_WAKE_WORD_STOP);
}

void HermesVoice::begin_capture_() {
  if (!this->reset_turn_state_(false)) {
    this->set_error_("input_reset_failed", "Could not reset the bounded microphone buffer");
    return;
  }
  const uint64_t turn_id = this->generate_turn_id_();
  this->set_active_turn_id_(turn_id);
  this->turn_active_.store(true, std::memory_order_release);
  this->turn_start_requested_.store(true, std::memory_order_release);
  this->capture_started_ms_ = millis();
  this->turn_started_ms_ = this->capture_started_ms_;
  this->speech_start_triggered_ = false;
  this->microphone_source_->start();
  this->set_state_(State::STARTING_MICROPHONE);
  this->notify_sender_();
}

void HermesVoice::commit_turn_() {
  // This sequence participates in the callback quiescence protocol used by
  // the sender task. A callback either latches capture before this exchange
  // and is awaited, or observes false and cannot append command audio.
  if (!this->capturing_.exchange(false))
    return;
  this->commit_requested_.store(true, std::memory_order_release);
  this->set_state_(State::PROCESSING);
  this->thinking_trigger_.trigger();
  this->notify_sender_();
}

void HermesVoice::cancel_active_turn_(const char *reason) {
  const uint64_t turn_id = this->get_active_turn_id_();
  this->turn_active_.store(false, std::memory_order_release);
  this->turn_ready_.store(false, std::memory_order_release);
  this->turn_start_requested_.store(false, std::memory_order_release);
  this->commit_requested_.store(false, std::memory_order_release);
  this->capturing_.store(false, std::memory_order_release);
  this->barge_monitoring_.store(false, std::memory_order_release);
  this->set_active_turn_id_(0);
  if (turn_id != 0 && this->websocket_connected_.load(std::memory_order_acquire)) {
    this->enqueue_control_("{\"v\":2,\"type\":\"turn.cancel\",\"turn_id\":\"%016" PRIx64 "\",\"reason\":\"%s\"}",
                           turn_id, reason);
  }
  this->notify_sender_();
}

void HermesVoice::stop() {
  if (this->state_ == State::IDLE)
    return;
  ESP_LOGD(TAG, "Cancelling active realtime turn");
  this->cancel_active_turn_("user");
  if (!this->reset_input_audio_())
    ESP_LOGW(TAG, "Could not reset microphone audio while stopping the turn");
  this->clear_output_audio_();
  this->speaker_->stop();
  this->microphone_source_->stop();
  this->set_state_(State::STOPPING_MICROPHONE);
}

void HermesVoice::new_conversation() {
  if (!this->configured_) {
    ESP_LOGW(TAG, "Hermes is not configured; no conversation exists to reset");
    return;
  }
  if (this->is_failed()) {
    ESP_LOGW(TAG, "Cannot start a new conversation because the component failed setup");
    return;
  }
  if (this->state_ != State::IDLE) {
    ESP_LOGW(TAG, "Cancel the active voice turn before starting a new conversation");
    return;
  }
  if (this->conversation_reset_pending_.load(std::memory_order_acquire)) {
    ESP_LOGD(TAG, "A new-conversation request is already pending");
    return;
  }

  const uint64_t request_id = this->generate_turn_id_();
  this->set_conversation_reset_request_id_(request_id);
  this->conversation_reset_pending_.store(true, std::memory_order_release);
  this->conversation_reset_send_requested_.store(true, std::memory_order_release);
  this->conversation_reset_sent_ms_.store(0, std::memory_order_release);
  this->conversation_reset_attempts_.store(0, std::memory_order_release);
  this->notify_sender_();
  ESP_LOGI(TAG, "Requesting a new Hermes conversation (request %016" PRIx64 ")", request_id);
}

void HermesVoice::begin_barge_in_() {
  const uint64_t old_turn_id = this->get_active_turn_id_();
  if (old_turn_id == 0)
    return;

  // Invalidate the old turn and clear its audio atomically with respect to the
  // WebSocket receive task. A frame which already passed validation is drained
  // before this reset; every later old-turn frame is dropped.
  {
    SemaphoreGuard lock(this->output_buffer_mutex_);
    if (!lock.locked()) {
      this->set_error_("output_lock_failed", "Could not lock realtime playback state");
      return;
    }
    this->turn_active_.store(false, std::memory_order_release);
    this->set_active_turn_id_(0);
    this->output_buffer_->reset();
  }
  this->enqueue_control_("{\"v\":2,\"type\":\"turn.cancel\",\"turn_id\":\"%016" PRIx64 "\",\"reason\":\"barge_in\"}",
                         old_turn_id);
  this->speaker_->stop();
  this->speaker_pending_size_ = 0;
  this->speaker_pending_offset_ = 0;
  if (this->micro_wake_word_ != nullptr)
    this->micro_wake_word_->stop();

  const uint32_t barge_started = this->barge_voice_started_ms_.load(std::memory_order_acquire);
  if (!this->reset_turn_state_(true)) {
    this->set_error_("input_reset_failed", "Could not preserve barge-in microphone audio");
    return;
  }
  const uint64_t new_turn_id = this->generate_turn_id_();
  this->set_active_turn_id_(new_turn_id);
  this->turn_active_.store(true, std::memory_order_release);
  this->turn_start_requested_.store(true, std::memory_order_release);
  this->capturing_.store(true, std::memory_order_release);
  this->speech_seen_.store(true, std::memory_order_release);
  this->last_voice_ms_.store(millis(), std::memory_order_release);
  this->capture_started_ms_ = barge_started == 0 ? millis() : barge_started;
  this->turn_started_ms_ = millis();
  this->speech_start_triggered_ = true;
  this->set_state_(State::LISTENING);
  this->listening_trigger_.trigger();
  this->speech_start_trigger_.trigger();
  this->notify_sender_();
}

void HermesVoice::finish_turn_() {
  this->turn_active_.store(false, std::memory_order_release);
  this->set_active_turn_id_(0);
  this->capturing_.store(false, std::memory_order_release);
  this->barge_monitoring_.store(false, std::memory_order_release);
  this->microphone_source_->stop();
  this->set_state_(State::STOPPING_MICROPHONE);
}

void HermesVoice::return_to_idle_() {
  this->turn_active_.store(false, std::memory_order_release);
  this->turn_start_requested_.store(false, std::memory_order_release);
  this->capturing_.store(false, std::memory_order_release);
  this->set_active_turn_id_(0);
  if (!this->reset_input_audio_())
    ESP_LOGW(TAG, "Could not reset microphone audio while returning idle");
  this->clear_output_audio_();
  this->release_realtime_wifi_();
  this->set_state_(State::IDLE);
  this->idle_trigger_.trigger();
}

void HermesVoice::set_error_(const char *code, const char *message) {
  ESP_LOGE(TAG, "%s: %s", code, message);
  if (this->state_ != State::IDLE && this->state_ != State::ERROR)
    this->cancel_active_turn_("error");
  this->capturing_.store(false, std::memory_order_release);
  this->barge_monitoring_.store(false, std::memory_order_release);
  this->microphone_source_->stop();
  this->speaker_->stop();
  if (!this->reset_input_audio_())
    ESP_LOGW(TAG, "Could not reset microphone audio after an error");
  this->clear_output_audio_();
  this->set_state_(State::ERROR);
  this->error_trigger_.trigger(code, message);
}

bool HermesVoice::try_enter_microphone_callback_() {
  portENTER_CRITICAL(&this->microphone_callback_mux_);
  const bool enabled = this->microphone_callbacks_enabled_;
  if (enabled)
    this->microphone_callbacks_inflight_.fetch_add(1, std::memory_order_acq_rel);
  portEXIT_CRITICAL(&this->microphone_callback_mux_);
  return enabled;
}

void HermesVoice::set_microphone_callbacks_enabled_(bool enabled) {
  portENTER_CRITICAL(&this->microphone_callback_mux_);
  this->microphone_callbacks_enabled_ = enabled;
  portEXIT_CRITICAL(&this->microphone_callback_mux_);
}

void HermesVoice::handle_microphone_data_(const std::vector<uint8_t> &data) {
  const size_t even_size = data.size() & ~static_cast<size_t>(1);
  if (even_size < sizeof(int16_t))
    return;

  // Admission and shutdown/OTA disable share one short critical section.
  // Once disable returns, no later callback can become in-flight or touch the
  // bounded rings; the barrier only has to drain callbacks already admitted.
  if (!this->try_enter_microphone_callback_())
    return;
  struct CallbackGuard {
    std::atomic<uint32_t> &counter;
    TaskHandle_t sender_task;
    ~CallbackGuard() {
      // Closing the final callback is part of the sender's commit predicate.
      // Notify after publishing 1 -> 0 so the sender cannot consume the audio
      // notification early, observe one in-flight callback, then sleep forever.
      if (this->counter.fetch_sub(1, std::memory_order_acq_rel) == 1 && this->sender_task != nullptr)
        xTaskNotifyGive(this->sender_task);
    }
  } callback_guard{this->microphone_callbacks_inflight_, this->sender_task_.get_handle()};
  // Latch after publishing the in-flight callback. Once commit_turn_ clears
  // capturing, no subsequently entering callback can write to the turn's ring.
  const bool capture_this_chunk = this->capturing_.load();
  const uint32_t callback_generation = this->capture_generation_.load(std::memory_order_acquire);

  const int16_t *samples = reinterpret_cast<const int16_t *>(data.data());
  const size_t sample_count = even_size / sizeof(int16_t);
  uint64_t sum_squares = 0;
  for (size_t i = 0; i < sample_count; i++) {
    const int32_t sample = samples[i];
    sum_squares += static_cast<uint64_t>(sample * sample);
  }
  const uint64_t mean_square = sum_squares / sample_count;
  const uint64_t threshold_square = static_cast<uint64_t>(this->silence_threshold_) * this->silence_threshold_;
  const bool voice = mean_square >= threshold_square;
  const uint32_t now = millis();

  if (capture_this_chunk) {
    SemaphoreGuard lock(this->input_buffer_mutex_, pdMS_TO_TICKS(5));
    if (!lock.locked()) {
      this->input_buffer_overflow_.store(true, std::memory_order_release);
      App.wake_loop_threadsafe();
      return;
    }
    if (callback_generation != this->capture_generation_.load(std::memory_order_acquire))
      return;
    const size_t written = this->input_buffer_->write_without_replacement(data.data(), even_size, 0, false);
    if (written != even_size) {
      this->input_buffer_overflow_.store(true, std::memory_order_release);
      App.wake_loop_threadsafe();
      return;
    }
    if (voice) {
      this->last_voice_ms_.store(now, std::memory_order_release);
      this->speech_seen_.store(true, std::memory_order_release);
    }
    this->notify_sender_();
    return;
  }

  if (!this->barge_monitoring_.load(std::memory_order_acquire) || !this->commit_sent_.load(std::memory_order_acquire))
    return;

  // With micro_wake_word configured, its channel-1 inference is the barge-in
  // detector and this channel-0 callback is only the new-turn capture path.
  // The energy detector remains as a bounded fallback for component users who
  // intentionally omit micro_wake_word.
  if (this->micro_wake_word_ != nullptr)
    return;

  if (!voice) {
    if (!this->barge_requested_.load(std::memory_order_acquire)) {
      const uint32_t abandoned_start = this->barge_voice_started_ms_.exchange(0, std::memory_order_acq_rel);
      if (abandoned_start != 0 && !this->reset_input_audio_()) {
        this->input_buffer_overflow_.store(true, std::memory_order_release);
        App.wake_loop_threadsafe();
      }
    }
    return;
  }

  uint32_t barge_started = this->barge_voice_started_ms_.load(std::memory_order_acquire);
  if (barge_started == 0) {
    if (!this->reset_input_audio_()) {
      this->input_buffer_overflow_.store(true, std::memory_order_release);
      App.wake_loop_threadsafe();
      return;
    }
    barge_started = now == 0 ? 1 : now;
    this->barge_voice_started_ms_.store(barge_started, std::memory_order_release);
  }
  const uint32_t barge_generation = this->capture_generation_.load(std::memory_order_acquire);
  SemaphoreGuard lock(this->input_buffer_mutex_, pdMS_TO_TICKS(5));
  if (!lock.locked()) {
    this->input_buffer_overflow_.store(true, std::memory_order_release);
    App.wake_loop_threadsafe();
    return;
  }
  if (!this->barge_monitoring_.load(std::memory_order_acquire) ||
      barge_generation != this->capture_generation_.load(std::memory_order_acquire))
    return;
  const size_t written = this->input_buffer_->write_without_replacement(data.data(), even_size, 0, false);
  if (written != even_size) {
    this->input_buffer_overflow_.store(true, std::memory_order_release);
    App.wake_loop_threadsafe();
    return;
  }
  if (now - barge_started >= BARGE_IN_SPEECH_MS)
    this->barge_requested_.store(true, std::memory_order_release);
  this->notify_sender_();
}

bool HermesVoice::enqueue_control_(const char *format, ...) {
  if (this->outbound_queue_ == nullptr)
    return false;
  OutboundControl control;
  va_list args;
  va_start(args, format);
  const int length = vsnprintf(control.data, sizeof(control.data), format, args);
  va_end(args);
  if (length <= 0 || static_cast<size_t>(length) >= sizeof(control.data)) {
    this->protocol_error_.store(true, std::memory_order_release);
    return false;
  }
  control.length = static_cast<uint16_t>(length);
  if (xQueueSend(this->outbound_queue_, &control, 0) != pdTRUE) {
    this->protocol_error_.store(true, std::memory_order_release);
    return false;
  }
  this->notify_sender_();
  return true;
}

bool HermesVoice::enqueue_inbound_(const InboundEvent &event) {
  if (this->inbound_queue_ == nullptr || xQueueSend(this->inbound_queue_, &event, 0) != pdTRUE) {
    this->protocol_error_.store(true, std::memory_order_release);
    return false;
  }
  App.wake_loop_threadsafe();
  this->notify_sender_();
  return true;
}

bool HermesVoice::acknowledge_output_through_(uint64_t turn_id, uint32_t sequence) {
  if (turn_id == 0 || sequence == UINT32_MAX)
    return false;
  const uint32_t acknowledged_count = sequence + 1;
  if (acknowledged_count <= this->acknowledged_output_count_.load(std::memory_order_acquire))
    return true;
  if (!this->enqueue_control_("{\"v\":2,\"type\":\"output.ack\",\"turn_id\":\"%016" PRIx64 "\",\"seq\":%" PRIu32 "}",
                              turn_id, sequence))
    return false;
  this->acknowledged_output_count_.store(acknowledged_count, std::memory_order_release);
  return true;
}

bool HermesVoice::initialize_websocket_() {
  this->handshake_headers_ =
      "Authorization: Bearer " + this->auth_token_ + "\r\nX-Device-ID: " + this->device_id_ + "\r\n";
  esp_websocket_client_config_t config = {};
  config.uri = this->gateway_url_.c_str();
  // The dedicated sender/supervisor task owns every start/stop transition.
  // Disabling the client's independent reconnect loop gives one bounded,
  // observable transport epoch and one jittered backoff policy.
  config.disable_auto_reconnect = true;
  config.enable_close_reconnect = false;
  config.user_context = this;
  config.task_prio = 5;
  config.task_stack = 6144;
  config.buffer_size = 4096;
  config.subprotocol = REALTIME_SUBPROTOCOL;
  config.user_agent = FIRMWARE_VERSION;
  config.headers = this->handshake_headers_.c_str();
  config.pingpong_timeout_sec = 30;
  config.reconnect_timeout_ms = RECONNECT_INITIAL_DELAY_MS;
  config.network_timeout_ms = 2000;
  config.ping_interval_sec = 10;
#if CONFIG_MBEDTLS_CERTIFICATE_BUNDLE
  config.crt_bundle_attach = esp_crt_bundle_attach;
#endif

  this->websocket_ = esp_websocket_client_init(&config);
  if (this->websocket_ == nullptr)
    return false;
  if (esp_websocket_register_events(this->websocket_, WEBSOCKET_EVENT_ANY, websocket_event_handler_, this) != ESP_OK) {
    esp_websocket_client_destroy(this->websocket_);
    this->websocket_ = nullptr;
    return false;
  }
  return true;
}

void HermesVoice::resume_transport() {
  this->transport_allowed_.store(true, std::memory_order_release);
  if (!this->configured_ || this->websocket_ == nullptr || this->shutting_down_.load(std::memory_order_acquire))
    return;
  this->notify_sender_();
  ESP_LOGD(TAG, "Realtime transport permitted after BLE shutdown");
}

bool HermesVoice::start_transport_epoch_() {
  if (this->websocket_ == nullptr || this->transport_started_.load(std::memory_order_acquire))
    return false;
  this->reset_transport_epoch_state_();
  uint32_t epoch = this->transport_epoch_.fetch_add(1, std::memory_order_acq_rel) + 1;
  if (epoch == 0) {
    this->transport_epoch_.store(1, std::memory_order_release);
    epoch = 1;
  }
  this->connected_epoch_.store(epoch, std::memory_order_release);
  this->transport_quiesced_.store(false, std::memory_order_release);
  // Publish first: an immediate handshake failure can dispatch before start()
  // returns, and its event must refer to this epoch rather than the prior one.
  this->transport_started_.store(true, std::memory_order_release);
  if (esp_websocket_client_start(this->websocket_) != ESP_OK) {
    this->transport_started_.store(false, std::memory_order_release);
    this->transport_quiesced_.store(true, std::memory_order_release);
    return false;
  }
  ESP_LOGD(TAG, "Started realtime transport epoch %" PRIu32, epoch);
  return true;
}

void HermesVoice::suspend_transport() {
  this->transport_allowed_.store(false, std::memory_order_release);
  this->notify_sender_();
}

void HermesVoice::stop_transport_epoch_() {
  // Only the sender/supervisor task calls ESP-IDF's synchronous stop. This is
  // the ownership barrier before parser and queue state are reused.
  if (this->websocket_ != nullptr && this->transport_started_.load(std::memory_order_acquire)) {
    const uint32_t stop_started = millis();
    this->transport_stop_reboot_requested_.store(false, std::memory_order_release);
    this->transport_stop_started_ms_.store(stop_started == 0 ? 1 : stop_started, std::memory_order_release);
    esp_websocket_client_stop(this->websocket_);
    this->transport_stop_started_ms_.store(0, std::memory_order_release);
  }
  this->transport_started_.store(false, std::memory_order_release);
  this->reset_transport_epoch_state_();
  this->transport_quiesced_.store(true, std::memory_order_release);
}

void HermesVoice::request_transport_restart_() {
  this->transport_restart_requested_.store(true, std::memory_order_release);
  this->notify_sender_();
}

void HermesVoice::reset_transport_epoch_state_() {
  this->websocket_connected_.store(false, std::memory_order_release);
  this->protocol_ready_.store(false, std::memory_order_release);
  this->hello_pending_.store(false, std::memory_order_release);
  this->connected_started_ms_.store(0, std::memory_order_release);
  this->hello_sent_ms_.store(0, std::memory_order_release);
  this->protocol_ready_since_ms_.store(0, std::memory_order_release);
  this->connected_epoch_.store(0, std::memory_order_release);
  if (this->conversation_reset_pending_.load(std::memory_order_acquire)) {
    this->conversation_reset_sent_ms_.store(0, std::memory_order_release);
    this->conversation_reset_attempts_.store(0, std::memory_order_release);
    this->conversation_reset_send_requested_.store(true, std::memory_order_release);
  }
  if (this->outbound_queue_ != nullptr)
    xQueueReset(this->outbound_queue_);
  if (this->inbound_queue_ != nullptr)
    xQueueReset(this->inbound_queue_);
  this->reset_inbound_message_();
}

void HermesVoice::notify_sender_() {
  TaskHandle_t task = this->sender_task_.get_handle();
  if (task != nullptr)
    xTaskNotifyGive(task);
}

void HermesVoice::prepare_for_ota_() {
  if (this->ota_active_.exchange(true, std::memory_order_acq_rel))
    return;
  ESP_LOGI(TAG, "Quiescing realtime audio and transport for OTA");
  this->transport_allowed_before_ota_.store(this->transport_allowed_.exchange(false, std::memory_order_acq_rel),
                                            std::memory_order_release);
  if (this->turn_active_.load(std::memory_order_acquire))
    this->cancel_active_turn_("ota");
  this->capturing_.store(false, std::memory_order_release);
  this->barge_monitoring_.store(false, std::memory_order_release);
  this->set_microphone_callbacks_enabled_(false);
  this->microphone_source_->stop();
  this->speaker_->stop();
  this->reset_input_audio_();
  this->clear_output_audio_();
  this->release_realtime_wifi_();
  this->set_state_(State::IDLE);
  this->idle_trigger_.trigger();
  this->notify_sender_();

  // ESPHome invokes OTA_STARTED listeners synchronously immediately before
  // backend_->begin(). Do not return into flash setup until transport and
  // microphone ownership have quiesced. Speaker stop is already queued, but
  // its state transition runs on this same loop and therefore cannot be part
  // of this synchronous predicate. If the IDF stop wedges, a synchronous safe
  // reboot aborts this OTA attempt before backend begin rather than writing
  // while the old realtime epoch is still active.
  const uint32_t wait_started = millis();
  while (this->transport_started_.load(std::memory_order_acquire) ||
         !this->transport_quiesced_.load(std::memory_order_acquire) ||
         !this->microphone_source_->is_stopped() ||
         this->microphone_callbacks_inflight_.load(std::memory_order_acquire) != 0) {
    if (millis() - wait_started >= TRANSPORT_STOP_WAIT_MS) {
      ESP_LOGE(TAG, "Realtime audio/transport did not quiesce before OTA; rebooting without starting flash");
      App.safe_reboot();
      return;
    }
    App.feed_wdt();
    delay(10);
  }
}

void HermesVoice::recover_from_ota_() {
  if (!this->ota_active_.exchange(false, std::memory_order_acq_rel))
    return;
  ESP_LOGW(TAG, "OTA did not complete; restoring realtime transport policy");
  this->set_microphone_callbacks_enabled_(true);
  if (this->transport_allowed_before_ota_.exchange(false, std::memory_order_acq_rel)) {
    this->transport_allowed_.store(true, std::memory_order_release);
    this->notify_sender_();
  }
}

void HermesVoice::cleanup_resources_() {
  if (this->transport_started_.load(std::memory_order_acquire))
    return;
  this->sender_task_.deallocate();
  if (this->websocket_ != nullptr) {
    esp_websocket_unregister_events(this->websocket_, WEBSOCKET_EVENT_ANY, websocket_event_handler_);
    esp_websocket_client_destroy(this->websocket_);
    this->websocket_ = nullptr;
  }
  if (this->outbound_queue_ != nullptr) {
    vQueueDelete(this->outbound_queue_);
    this->outbound_queue_ = nullptr;
  }
  if (this->inbound_queue_ != nullptr) {
    vQueueDelete(this->inbound_queue_);
    this->inbound_queue_ = nullptr;
  }
  if (this->output_credit_queue_ != nullptr) {
    vQueueDelete(this->output_credit_queue_);
    this->output_credit_queue_ = nullptr;
  }
  if (this->input_buffer_mutex_ != nullptr) {
    vSemaphoreDelete(this->input_buffer_mutex_);
    this->input_buffer_mutex_ = nullptr;
  }
  if (this->output_buffer_mutex_ != nullptr) {
    vSemaphoreDelete(this->output_buffer_mutex_);
    this->output_buffer_mutex_ = nullptr;
  }
  if (this->inbound_message_buffer_ != nullptr) {
    RAMAllocator<uint8_t> external_allocator(RAMAllocator<uint8_t>::ALLOC_EXTERNAL);
    external_allocator.deallocate(this->inbound_message_buffer_, MAX_INBOUND_MESSAGE_BYTES);
    this->inbound_message_buffer_ = nullptr;
  }
  this->input_buffer_.reset();
  this->output_buffer_.reset();
}

void HermesVoice::websocket_event_handler_(void *handler_args, esp_event_base_t base, int32_t event_id,
                                           void *event_data) {
  auto *self = static_cast<HermesVoice *>(handler_args);
  if (self != nullptr)
    self->handle_websocket_event_(event_id, static_cast<esp_websocket_event_data_t *>(event_data));
}

void HermesVoice::handle_websocket_event_(int32_t event_id, esp_websocket_event_data_t *event) {
  switch (event_id) {
    case WEBSOCKET_EVENT_CONNECTED:
      ESP_LOGI(TAG, "Realtime gateway WebSocket connected");
      this->reset_inbound_message_();
      this->protocol_ready_.store(false, std::memory_order_release);
      this->connected_started_ms_.store(millis(), std::memory_order_release);
      this->hello_sent_ms_.store(0, std::memory_order_release);
      this->websocket_connected_.store(true, std::memory_order_release);
      this->hello_pending_.store(true, std::memory_order_release);
      if (this->conversation_reset_pending_.load(std::memory_order_acquire))
        this->conversation_reset_send_requested_.store(true, std::memory_order_release);
      this->notify_sender_();
      break;
    case WEBSOCKET_EVENT_DISCONNECTED:
    case WEBSOCKET_EVENT_CLOSED:
      this->websocket_connected_.store(false, std::memory_order_release);
      this->protocol_ready_.store(false, std::memory_order_release);
      this->hello_pending_.store(false, std::memory_order_release);
      this->connected_started_ms_.store(0, std::memory_order_release);
      this->hello_sent_ms_.store(0, std::memory_order_release);
      this->protocol_ready_since_ms_.store(0, std::memory_order_release);
      if (this->conversation_reset_pending_.load(std::memory_order_acquire))
        this->conversation_reset_send_requested_.store(true, std::memory_order_release);
      if (this->turn_active_.load(std::memory_order_acquire))
        this->connection_lost_.store(true, std::memory_order_release);
      if (this->transport_allowed_.load(std::memory_order_acquire) &&
          !this->shutting_down_.load(std::memory_order_acquire) && !this->ota_active_.load(std::memory_order_acquire))
        this->transport_restart_requested_.store(true, std::memory_order_release);
      App.wake_loop_threadsafe();
      this->notify_sender_();
      break;
    case WEBSOCKET_EVENT_DATA:
      if (event != nullptr)
        this->handle_websocket_data_(event);
      break;
    case WEBSOCKET_EVENT_ERROR:
      if (event != nullptr) {
        ESP_LOGW(TAG, "Realtime WebSocket error (type=%d, HTTP=%d)", event->error_handle.error_type,
                 event->error_handle.esp_ws_handshake_status_code);
      }
      if (this->transport_allowed_.load(std::memory_order_acquire) &&
          !this->shutting_down_.load(std::memory_order_acquire))
        this->transport_restart_requested_.store(true, std::memory_order_release);
      this->notify_sender_();
      break;
    case WEBSOCKET_EVENT_FINISH:
      this->transport_started_.store(false, std::memory_order_release);
      if (this->transport_allowed_.load(std::memory_order_acquire) &&
          !this->shutting_down_.load(std::memory_order_acquire)) {
        this->transport_restart_requested_.store(true, std::memory_order_release);
      }
      this->notify_sender_();
      break;
    default:
      break;
  }
}

void HermesVoice::reset_inbound_message_() {
  this->inbound_message_size_ = 0;
  this->inbound_message_opcode_ = 0;
  this->inbound_message_active_ = false;
}

void HermesVoice::handle_websocket_data_(const esp_websocket_event_data_t *event) {
  const uint8_t opcode = event->op_code;
  if (opcode == WS_OPCODE_CLOSE || opcode == WS_OPCODE_PING || opcode == WS_OPCODE_PONG)
    return;
  if (event->data_len < 0 || event->payload_len < 0 || event->payload_offset < 0 || event->data_ptr == nullptr) {
    this->protocol_error_.store(true, std::memory_order_release);
    return;
  }

  if ((opcode == WS_OPCODE_TEXT || opcode == WS_OPCODE_BINARY) && event->payload_offset == 0) {
    if (this->inbound_message_active_) {
      this->protocol_error_.store(true, std::memory_order_release);
      this->reset_inbound_message_();
    }
    this->inbound_message_active_ = true;
    this->inbound_message_opcode_ = opcode;
  } else if (opcode == WS_OPCODE_CONTINUATION) {
    if (!this->inbound_message_active_) {
      this->protocol_error_.store(true, std::memory_order_release);
      return;
    }
  } else if (!this->inbound_message_active_) {
    this->protocol_error_.store(true, std::memory_order_release);
    return;
  }

  const size_t data_len = static_cast<size_t>(event->data_len);
  if (this->inbound_message_size_ + data_len > MAX_INBOUND_MESSAGE_BYTES) {
    this->protocol_error_.store(true, std::memory_order_release);
    this->reset_inbound_message_();
    return;
  }
  memcpy(this->inbound_message_buffer_ + this->inbound_message_size_, event->data_ptr, data_len);
  this->inbound_message_size_ += data_len;

  const bool frame_complete = event->payload_offset + event->data_len >= event->payload_len;
  if (frame_complete && event->fin) {
    this->process_inbound_message_(this->inbound_message_opcode_, this->inbound_message_buffer_,
                                   this->inbound_message_size_);
    this->reset_inbound_message_();
  }
}

void HermesVoice::process_inbound_message_(uint8_t opcode, const uint8_t *data, size_t size) {
  if (opcode == WS_OPCODE_TEXT) {
    this->process_control_message_(data, size);
  } else if (opcode == WS_OPCODE_BINARY) {
    this->process_output_audio_(data, size);
  } else {
    this->protocol_error_.store(true, std::memory_order_release);
  }
}

void HermesVoice::process_control_message_(const uint8_t *data, size_t size) {
  bool recognized = false;
  const bool parsed = json::parse_json(data, size, [this, &recognized](JsonObject root) {
    if ((root["v"] | 0) != PROTOCOL_VERSION)
      return false;
    const char *type = root["type"].as<const char *>();
    if (type == nullptr)
      return false;

    InboundEvent event;
    event.transport_epoch = this->connected_epoch_.load(std::memory_order_acquire);
    if (event.transport_epoch == 0)
      return false;
    if (strcmp(type, "ready") == 0) {
      const char *input_format = root["input_format"].as<const char *>();
      const char *output_format = root["output_format"].as<const char *>();
      if (!root["input_window"].is<uint32_t>() || root["input_window"].as<uint32_t>() != INPUT_WINDOW_FRAMES ||
          !root["output_window"].is<uint32_t>() || root["output_window"].as<uint32_t>() != OUTPUT_WINDOW_FRAMES ||
          !root["frame_ms"].is<uint32_t>() || root["frame_ms"].as<uint32_t>() != AUDIO_FRAME_DURATION_MS ||
          input_format == nullptr || output_format == nullptr || strcmp(input_format, PCM_FORMAT) != 0 ||
          strcmp(output_format, PCM_FORMAT) != 0)
        return false;
      if (!root["conversation_id"].isNull()) {
        const char *conversation_id = root["conversation_id"].as<const char *>();
        if (!copy_conversation_id(conversation_id, event.conversation_id, sizeof(event.conversation_id)))
          return false;
      }
      event.type = InboundEventType::READY;
      recognized = true;
      return this->enqueue_inbound_(event);
    }

    if (strcmp(type, "conversation.reset.done") == 0) {
      const char *request_text = root["request_id"].as<const char *>();
      const char *conversation_id = root["conversation_id"].as<const char *>();
      if (!parse_request_id(request_text, event.request_id) ||
          !copy_conversation_id(conversation_id, event.conversation_id, sizeof(event.conversation_id)))
        return false;
      event.type = InboundEventType::CONVERSATION_RESET_DONE;
      recognized = true;
      return this->enqueue_inbound_(event);
    }

    const char *turn_text = root["turn_id"].as<const char *>();
    if (strcmp(type, "error") != 0 && !parse_turn_id(turn_text, event.turn_id))
      return false;
    if (strcmp(type, "error") == 0 && turn_text != nullptr && !parse_turn_id(turn_text, event.turn_id))
      return false;

    if (strcmp(type, "turn.ready") == 0) {
      event.type = InboundEventType::TURN_READY;
    } else if (strcmp(type, "input.ack") == 0) {
      if (!root["seq"].is<uint32_t>())
        return false;
      event.type = InboundEventType::INPUT_ACK;
      event.sequence = root["seq"].as<uint32_t>();
    } else if (strcmp(type, "transcript.final") == 0) {
      event.type = InboundEventType::TRANSCRIPT_FINAL;
    } else if (strcmp(type, "response.start") == 0) {
      event.type = InboundEventType::RESPONSE_START;
    } else if (strcmp(type, "tts.start") == 0) {
      if (!root["sample_rate"].is<uint32_t>())
        return false;
      event.type = InboundEventType::TTS_START;
      event.value = root["sample_rate"].as<uint32_t>();
    } else if (strcmp(type, "tts.end") == 0) {
      if (!root["last_seq"].is<uint32_t>())
        return false;
      event.type = InboundEventType::TTS_END;
      event.sequence = root["last_seq"].as<uint32_t>();
    } else if (strcmp(type, "turn.done") == 0) {
      event.type = InboundEventType::TURN_DONE;
    } else if (strcmp(type, "error") == 0) {
      event.type = InboundEventType::ERROR;
      const char *code = root["code"].as<const char *>();
      const char *message = root["message"].as<const char *>();
      snprintf(event.code, sizeof(event.code), "%s", code == nullptr ? "gateway_error" : code);
      snprintf(event.message, sizeof(event.message), "%s", message == nullptr ? "Realtime gateway error" : message);
      event.fatal = root["fatal"] | false;
    } else {
      // Unknown v2 controls are forward-compatible and can be ignored.
      recognized = true;
      return true;
    }
    recognized = true;
    return this->enqueue_inbound_(event);
  });

  if (!parsed || !recognized)
    this->protocol_error_.store(true, std::memory_order_release);
}

void HermesVoice::process_output_audio_(const uint8_t *data, size_t size) {
  if (size <= BINARY_HEADER_SIZE || data[0] != PROTOCOL_VERSION || data[1] != OUTPUT_AUDIO_KIND || data[2] != 0 ||
      data[3] != BINARY_HEADER_SIZE) {
    this->protocol_error_.store(true, std::memory_order_release);
    return;
  }
  const size_t payload_size = size - BINARY_HEADER_SIZE;
  // Protocol v2 grants credit in 64 ms units. Accepting a larger binary frame
  // would let 32 advertised credits exceed the bounded playback ring.
  if ((payload_size & 1U) != 0 || payload_size > AUDIO_FRAME_BYTES) {
    this->protocol_error_.store(true, std::memory_order_release);
    return;
  }

  const uint64_t turn_id = read_u64_be(data + 4);
  SemaphoreGuard lock(this->output_buffer_mutex_);
  if (!lock.locked()) {
    this->protocol_error_.store(true, std::memory_order_release);
    return;
  }
  if (!this->turn_active_.load(std::memory_order_acquire) || turn_id != this->get_active_turn_id_()) {
    ESP_LOGV(TAG, "Dropping late output audio for turn %016" PRIx64, turn_id);
    return;
  }
  const uint32_t sequence = read_u32_be(data + 12);
  const uint32_t first_sample = read_u32_be(data + 16);
  if (this->output_validation_turn_id_ != turn_id) {
    this->output_validation_turn_id_ = turn_id;
    this->expected_output_sequence_ = 0;
    this->expected_output_sample_ = 0;
    this->output_short_frame_seen_ = false;
  }
  if (sequence != this->expected_output_sequence_ || first_sample != this->expected_output_sample_) {
    this->protocol_error_.store(true, std::memory_order_release);
    return;
  }
  if (this->output_short_frame_seen_) {
    this->protocol_error_.store(true, std::memory_order_release);
    return;
  }
  if (uxQueueSpacesAvailable(this->output_credit_queue_) == 0) {
    this->output_buffer_overflow_.store(true, std::memory_order_release);
    return;
  }

  const size_t written =
      this->output_buffer_->write_without_replacement(data + BINARY_HEADER_SIZE, payload_size, 0, false);
  if (written != payload_size) {
    this->output_buffer_overflow_.store(true, std::memory_order_release);
    return;
  }
  const OutputFrameCredit credit{sequence, static_cast<uint32_t>(payload_size)};
  if (xQueueSend(this->output_credit_queue_, &credit, 0) != pdTRUE) {
    this->output_buffer_overflow_.store(true, std::memory_order_release);
    return;
  }
  this->expected_output_sequence_++;
  this->expected_output_sample_ += static_cast<uint32_t>(payload_size / sizeof(int16_t));
  this->output_short_frame_seen_ = payload_size < AUDIO_FRAME_BYTES;
  this->accepted_output_count_.store(sequence + 1, std::memory_order_release);
  this->last_output_sequence_.store(sequence, std::memory_order_release);
  App.wake_loop_threadsafe();
}

bool HermesVoice::start_sender_task_() {
  return this->sender_task_.create(sender_task_entry_, "hermes_sender", SENDER_TASK_STACK_SIZE, this,
                                   SENDER_TASK_PRIORITY, this->task_stack_in_psram_);
}

void HermesVoice::sender_task_entry_(void *parameter) {
  auto *self = static_cast<HermesVoice *>(parameter);
  self->run_sender_task_();
  vTaskSuspend(nullptr);
}

bool HermesVoice::send_text_(const char *data, size_t size) {
  if (this->websocket_ == nullptr || !this->websocket_connected_.load(std::memory_order_acquire))
    return false;
  const int sent = esp_websocket_client_send_text(this->websocket_, data, static_cast<int>(size), pdMS_TO_TICKS(250));
  return sent == static_cast<int>(size);
}

bool HermesVoice::send_binary_(const uint8_t *data, size_t size) {
  if (this->websocket_ == nullptr || !this->websocket_connected_.load(std::memory_order_acquire))
    return false;
  const int sent = esp_websocket_client_send_bin(this->websocket_, reinterpret_cast<const char *>(data),
                                                 static_cast<int>(size), pdMS_TO_TICKS(250));
  return sent == static_cast<int>(size);
}

void HermesVoice::run_sender_task_() {
  uint64_t sender_turn_id = 0;
  uint32_t next_sequence = 0;
  uint32_t next_sample = 0;
  uint32_t next_connect_at = 0;
  bool retry_scheduled = false;
  std::array<uint8_t, BINARY_HEADER_SIZE + AUDIO_FRAME_BYTES> frame{};

  const auto schedule_retry = [this, &next_connect_at, &retry_scheduled](uint32_t now) {
    const uint32_t base = this->reconnect_base_delay_ms_.load(std::memory_order_acquire);
    const uint32_t spread = std::max<uint32_t>(1, base / 4);
    const uint32_t jitter_range = spread * 2 + 1;
    const int32_t jitter = static_cast<int32_t>(esp_random() % jitter_range) - static_cast<int32_t>(spread);
    const uint32_t retry_delay = static_cast<uint32_t>(static_cast<int32_t>(base) + jitter);
    const uint32_t next = base >= RECONNECT_MAX_DELAY_MS / 2 ? RECONNECT_MAX_DELAY_MS : base * 2;
    this->reconnect_base_delay_ms_.store(next, std::memory_order_release);
    next_connect_at = now + retry_delay;
    retry_scheduled = true;
    ESP_LOGW(TAG, "Starting a fresh realtime epoch in %" PRIu32 " ms", retry_delay);
  };
  const auto fail_send = [this]() {
    this->send_failed_.store(true, std::memory_order_release);
    this->request_transport_restart_();
    App.wake_loop_threadsafe();
  };

  while (true) {
    if (this->shutting_down_.load(std::memory_order_acquire)) {
      this->stop_transport_epoch_();
      this->shutdown_complete_.store(true, std::memory_order_release);
      return;
    }

    const uint32_t now = millis();
    const bool transport_permitted = this->transport_allowed_.load(std::memory_order_acquire) &&
                                     !this->ota_active_.load(std::memory_order_acquire) &&
                                     this->tls_time_valid_.load(std::memory_order_acquire) && network::is_connected();

    if (!transport_permitted) {
      if (this->transport_started_.load(std::memory_order_acquire) ||
          !this->transport_quiesced_.load(std::memory_order_acquire))
        this->stop_transport_epoch_();
      this->transport_restart_requested_.store(false, std::memory_order_release);
      retry_scheduled = false;
      if (!this->transport_allowed_.load(std::memory_order_acquire))
        this->reconnect_base_delay_ms_.store(RECONNECT_INITIAL_DELAY_MS, std::memory_order_release);
      ulTaskNotifyTake(pdTRUE, portMAX_DELAY);
      continue;
    }

    if (this->transport_restart_requested_.exchange(false, std::memory_order_acq_rel)) {
      this->stop_transport_epoch_();
      // stop() dispatches CLOSED/FINISH synchronously and those callbacks may
      // request another restart for the epoch being retired.
      this->transport_restart_requested_.store(false, std::memory_order_release);
      schedule_retry(now);
    }

    if (!this->transport_started_.load(std::memory_order_acquire)) {
      if (retry_scheduled && static_cast<int32_t>(now - next_connect_at) < 0) {
        const uint32_t remaining = next_connect_at - now;
        ulTaskNotifyTake(pdTRUE, pdMS_TO_TICKS(std::max<uint32_t>(1, remaining)));
        continue;
      }
      retry_scheduled = false;
      if (!this->start_transport_epoch_()) {
        schedule_retry(now);
        continue;
      }
    }

    bool did_work = false;
    if (this->hello_pending_.exchange(false, std::memory_order_acq_rel)) {
      char hello[320];
      const int length = snprintf(hello, sizeof(hello),
                                  "{\"v\":2,\"type\":\"hello\",\"firmware\":\"%s\",\"input\":\"%s\","
                                  "\"output\":\"%s\",\"barge_in\":true}",
                                  FIRMWARE_VERSION, PCM_FORMAT, PCM_FORMAT);
      if (length <= 0 || static_cast<size_t>(length) >= sizeof(hello) ||
          !this->send_text_(hello, static_cast<size_t>(length))) {
        fail_send();
        continue;
      }
      this->hello_sent_ms_.store(now, std::memory_order_release);
      did_work = true;
    }

    if (!this->protocol_ready_.load(std::memory_order_acquire)) {
      const uint32_t hello_sent = this->hello_sent_ms_.load(std::memory_order_acquire);
      if (hello_sent != 0 && now - hello_sent >= HANDSHAKE_TIMEOUT_MS) {
        this->handshake_timeout_.store(true, std::memory_order_release);
        this->request_transport_restart_();
        App.wake_loop_threadsafe();
        continue;
      }
      const TickType_t wait = hello_sent == 0
                                  ? portMAX_DELAY
                                  : pdMS_TO_TICKS(std::max<uint32_t>(1, HANDSHAKE_TIMEOUT_MS - (now - hello_sent)));
      ulTaskNotifyTake(pdTRUE, wait);
      continue;
    }

    if (this->conversation_reset_send_requested_.exchange(false, std::memory_order_acq_rel)) {
      if (this->conversation_reset_pending_.load(std::memory_order_acquire)) {
        const uint64_t request_id = this->get_conversation_reset_request_id_();
        char reset[128];
        const int length =
            snprintf(reset, sizeof(reset),
                     "{\"v\":2,\"type\":\"conversation.reset\",\"request_id\":\"%016" PRIx64 "\"}", request_id);
        if (request_id == 0 || length <= 0 || static_cast<size_t>(length) >= sizeof(reset) ||
            !this->send_text_(reset, static_cast<size_t>(length))) {
          fail_send();
          continue;
        }
        this->conversation_reset_attempts_.fetch_add(1, std::memory_order_acq_rel);
        this->conversation_reset_sent_ms_.store(now == 0 ? 1 : now, std::memory_order_release);
        ESP_LOGD(TAG, "New-conversation request sent (%016" PRIx64 ")", request_id);
        did_work = true;
      }
    }

    OutboundControl control;
    while (xQueueReceive(this->outbound_queue_, &control, 0) == pdTRUE) {
      if (!this->send_text_(control.data, control.length)) {
        fail_send();
        break;
      }
      did_work = true;
    }
    if (this->send_failed_.load(std::memory_order_acquire)) {
      continue;
    }

    const bool turn_active = this->turn_active_.load(std::memory_order_acquire);
    const uint64_t turn_id = this->get_active_turn_id_();
    if (turn_active && turn_id != 0 && turn_id != sender_turn_id) {
      sender_turn_id = turn_id;
      next_sequence = 0;
      next_sample = 0;
      this->sent_input_count_.store(0, std::memory_order_release);
      this->acked_input_count_.store(0, std::memory_order_release);
      did_work = true;
    }

    if (turn_active && turn_id != 0 && this->turn_start_requested_.load(std::memory_order_acquire)) {
      char start[128];
      const int length =
          snprintf(start, sizeof(start), "{\"v\":2,\"type\":\"turn.start\",\"turn_id\":\"%016" PRIx64 "\"}", turn_id);
      if (length <= 0 || static_cast<size_t>(length) >= sizeof(start) ||
          !this->send_text_(start, static_cast<size_t>(length))) {
        fail_send();
        continue;
      }
      this->turn_start_requested_.store(false, std::memory_order_release);
      did_work = true;
    }

    if (turn_active && turn_id != 0 && this->turn_ready_.load(std::memory_order_acquire)) {
      const bool capture_complete = this->commit_requested_.load(std::memory_order_acquire) &&
                                    !this->capturing_.load() && this->microphone_callbacks_inflight_.load() == 0;
      uint8_t burst = 0;
      while (burst < 4 &&
             next_sequence - this->acked_input_count_.load(std::memory_order_acquire) < INPUT_WINDOW_FRAMES) {
        size_t bytes = 0;
        {
          SemaphoreGuard lock(this->input_buffer_mutex_, pdMS_TO_TICKS(20));
          if (!lock.locked())
            break;
          const size_t available = this->input_buffer_->available();
          if (available < AUDIO_FRAME_BYTES && !(capture_complete && available > 0))
            break;
          const size_t requested = std::min(available, AUDIO_FRAME_BYTES) & ~static_cast<size_t>(1);
          bytes = this->input_buffer_->read(frame.data() + BINARY_HEADER_SIZE, requested, 0);
        }
        if (bytes == 0 || (bytes & 1U) != 0) {
          this->protocol_error_.store(true, std::memory_order_release);
          App.wake_loop_threadsafe();
          break;
        }
        frame[0] = PROTOCOL_VERSION;
        frame[1] = INPUT_AUDIO_KIND;
        frame[2] = 0;
        frame[3] = BINARY_HEADER_SIZE;
        write_u64_be(frame.data() + 4, turn_id);
        write_u32_be(frame.data() + 12, next_sequence);
        write_u32_be(frame.data() + 16, next_sample);
        const uint32_t permitted_count = next_sequence + 1;
        // A peer can ACK immediately on the other core once send succeeds.
        // Publish the permitted cumulative count before exposing the frame;
        // a failed send retires this transport epoch, so no rollback is safe
        // or necessary.
        this->sent_input_count_.store(permitted_count, std::memory_order_release);
        if (!this->send_binary_(frame.data(), BINARY_HEADER_SIZE + bytes)) {
          fail_send();
          break;
        }
        next_sequence = permitted_count;
        next_sample += static_cast<uint32_t>(bytes / sizeof(int16_t));
        burst++;
        did_work = true;
      }

      bool input_empty = false;
      {
        SemaphoreGuard lock(this->input_buffer_mutex_, pdMS_TO_TICKS(20));
        if (lock.locked())
          input_empty = this->input_buffer_->available() == 0;
      }
      if (capture_complete && input_empty && next_sequence > 0 && !this->send_failed_.load(std::memory_order_acquire)) {
        char commit[160];
        const int length =
            snprintf(commit, sizeof(commit),
                     "{\"v\":2,\"type\":\"turn.commit\",\"turn_id\":\"%016" PRIx64 "\",\"last_seq\":%" PRIu32 "}",
                     turn_id, next_sequence - 1);
        if (length <= 0 || static_cast<size_t>(length) >= sizeof(commit) ||
            !this->send_text_(commit, static_cast<size_t>(length))) {
          fail_send();
          continue;
        }
        this->commit_requested_.store(false, std::memory_order_release);
        this->commit_sent_.store(true, std::memory_order_release);
        this->turn_ready_.store(false, std::memory_order_release);
        did_work = true;
      } else if (capture_complete && input_empty && next_sequence == 0) {
        this->protocol_error_.store(true, std::memory_order_release);
        App.wake_loop_threadsafe();
      }
    }

    if (did_work) {
      taskYIELD();
      continue;
    }
    ulTaskNotifyTake(pdTRUE, portMAX_DELAY);
  }
}

void HermesVoice::process_inbound_events_() {
  InboundEvent event;
  while (xQueueReceive(this->inbound_queue_, &event, 0) == pdTRUE) {
    const uint32_t connected_epoch = this->connected_epoch_.load(std::memory_order_acquire);
    if (event.transport_epoch == 0 || event.transport_epoch != connected_epoch) {
      ESP_LOGV(TAG, "Ignoring control from retired transport epoch %" PRIu32, event.transport_epoch);
      continue;
    }
    if (event.type == InboundEventType::READY) {
      if (event.conversation_id[0] != '\0' && this->conversation_id_ != event.conversation_id) {
        this->conversation_id_ = event.conversation_id;
        ESP_LOGD(TAG, "Hermes conversation state updated");
      }
      const bool was_ready = this->protocol_ready_.exchange(true, std::memory_order_acq_rel);
      this->protocol_ready_since_ms_.store(millis(), std::memory_order_release);
      if (this->conversation_reset_pending_.load(std::memory_order_acquire))
        this->conversation_reset_send_requested_.store(true, std::memory_order_release);
      this->notify_sender_();
      ESP_LOGI(TAG, "Realtime protocol v2 ready (epoch %" PRIu32 ")", connected_epoch);
      if (!was_ready && !this->conversation_reset_pending_.load(std::memory_order_acquire) &&
          this->state_ == State::IDLE)
        this->idle_trigger_.trigger();
      continue;
    }

    if (event.type == InboundEventType::CONVERSATION_RESET_DONE) {
      const uint64_t expected = this->get_conversation_reset_request_id_();
      if (!this->conversation_reset_pending_.load(std::memory_order_acquire) || event.request_id != expected) {
        ESP_LOGE(TAG, "Received an unsolicited or mismatched new-conversation acknowledgement");
        this->protocol_error_.store(true, std::memory_order_release);
        continue;
      }
      this->conversation_id_ = event.conversation_id;
      this->conversation_reset_send_requested_.store(false, std::memory_order_release);
      this->conversation_reset_sent_ms_.store(0, std::memory_order_release);
      this->conversation_reset_attempts_.store(0, std::memory_order_release);
      this->set_conversation_reset_request_id_(0);
      this->conversation_reset_pending_.store(false, std::memory_order_release);
      ESP_LOGI(TAG, "New Hermes conversation is ready");
      // A wake-word detection may have stopped itself while the reset was in
      // flight. Re-run the ordinary idle automation only after the durable
      // gateway acknowledgement makes starting the next turn safe.
      this->idle_trigger_.trigger();
      continue;
    }

    const uint64_t active_turn = this->get_active_turn_id_();
    if (event.type != InboundEventType::ERROR && (event.turn_id == 0 || event.turn_id != active_turn)) {
      ESP_LOGV(TAG, "Ignoring late control for turn %016" PRIx64, event.turn_id);
      continue;
    }
    if (event.type == InboundEventType::ERROR && !event.fatal && event.turn_id != 0 && event.turn_id != active_turn)
      continue;

    switch (event.type) {
      case InboundEventType::TURN_READY:
        this->turn_ready_.store(true, std::memory_order_release);
        this->notify_sender_();
        break;
      case InboundEventType::INPUT_ACK: {
        if (event.sequence == UINT32_MAX) {
          this->protocol_error_.store(true, std::memory_order_release);
          break;
        }
        const uint32_t acked_count = event.sequence + 1;
        const uint32_t sent_count = this->sent_input_count_.load(std::memory_order_acquire);
        if (acked_count > sent_count) {
          this->protocol_error_.store(true, std::memory_order_release);
          break;
        }
        uint32_t current = this->acked_input_count_.load(std::memory_order_relaxed);
        while (acked_count > current &&
               !this->acked_input_count_.compare_exchange_weak(current, acked_count, std::memory_order_release,
                                                               std::memory_order_relaxed)) {
        }
        this->notify_sender_();
        break;
      }
      case InboundEventType::TRANSCRIPT_FINAL:
        ESP_LOGD(TAG, "Final transcript received for active turn");
        break;
      case InboundEventType::RESPONSE_START:
        break;
      case InboundEventType::TTS_START:
        if (event.value != this->output_sample_rate_) {
          this->request_transport_restart_();
          this->set_error_("unsupported_audio_format", "Gateway selected an unsupported TTS sample rate");
          return;
        }
        this->tts_started_ = true;
        this->speaker_->set_audio_stream_info(audio::AudioStreamInfo(16, 1, this->output_sample_rate_));
        break;
      case InboundEventType::TTS_END: {
        const uint32_t accepted = this->accepted_output_count_.load(std::memory_order_acquire);
        const uint32_t last_sequence = this->last_output_sequence_.load(std::memory_order_acquire);
        if (event.sequence == UINT32_MAX || accepted == 0 || accepted - 1 != event.sequence ||
            event.sequence != last_sequence) {
          this->request_transport_restart_();
          this->set_error_("output_sequence_error", "TTS ended with a non-contiguous output sequence");
          return;
        }
        this->tts_ended_ = true;
        const uint32_t consumed = this->consumed_output_count_.load(std::memory_order_acquire);
        if (consumed > 0 && consumed - 1 == last_sequence) {
          // Credit advances only after PCM has left the network ring for the
          // bounded speaker path, never merely because a frame arrived. The
          // cumulative count suppresses a duplicate when this was also a
          // periodic boundary.
          this->acknowledge_output_through_(active_turn, last_sequence);
        }
        break;
      }
      case InboundEventType::TURN_DONE:
        this->remote_turn_done_ = true;
        if (!this->tts_started_)
          this->tts_ended_ = true;
        break;
      case InboundEventType::ERROR:
        if (event.turn_id == 0 && this->conversation_reset_pending_.load(std::memory_order_acquire)) {
          this->conversation_reset_send_requested_.store(false, std::memory_order_release);
          this->conversation_reset_sent_ms_.store(0, std::memory_order_release);
          this->conversation_reset_attempts_.store(0, std::memory_order_release);
          this->set_conversation_reset_request_id_(0);
          this->conversation_reset_pending_.store(false, std::memory_order_release);
          ESP_LOGW(TAG, "New-conversation request was rejected by the gateway");
        }
        this->set_error_(event.code[0] == '\0' ? "gateway_error" : event.code,
                         event.message[0] == '\0' ? "Realtime gateway error" : event.message);
        if (event.fatal)
          this->request_transport_restart_();
        return;
      case InboundEventType::READY:
      case InboundEventType::CONVERSATION_RESET_DONE:
        break;
    }
  }
}

void HermesVoice::service_playback_() {
  uint32_t output_ack_sequence = UINT32_MAX;
  bool credit_mismatch = false;
  if (this->speaker_pending_offset_ >= this->speaker_pending_size_) {
    this->speaker_pending_offset_ = 0;
    this->speaker_pending_size_ = 0;
    {
      SemaphoreGuard lock(this->output_buffer_mutex_);
      if (!lock.locked()) {
        credit_mismatch = true;
      } else {
        const size_t available = this->output_buffer_->available();
        if (available > 0) {
          const size_t requested = std::min(available, this->speaker_pending_.size()) & ~static_cast<size_t>(1);
          this->speaker_pending_size_ = this->output_buffer_->read(this->speaker_pending_.data(), requested, 0);

          size_t credited_bytes = this->speaker_pending_size_;
          while (credited_bytes > 0) {
            if (!this->output_credit_current_valid_) {
              if (xQueueReceive(this->output_credit_queue_, &this->output_credit_current_, 0) != pdTRUE) {
                credit_mismatch = true;
                break;
              }
              this->output_credit_current_valid_ = true;
            }
            const uint32_t consumed =
                static_cast<uint32_t>(std::min<size_t>(credited_bytes, this->output_credit_current_.remaining_bytes));
            if (consumed == 0) {
              credit_mismatch = true;
              break;
            }
            credited_bytes -= consumed;
            this->output_credit_current_.remaining_bytes -= consumed;
            if (this->output_credit_current_.remaining_bytes == 0) {
              const uint32_t completed_sequence = this->output_credit_current_.sequence;
              this->output_credit_current_valid_ = false;
              this->consumed_output_count_.store(completed_sequence + 1, std::memory_order_release);
              const bool periodic_ack = (completed_sequence + 1) % ACK_EVERY_FRAMES == 0;
              const bool final_ack =
                  this->tts_ended_ && completed_sequence == this->last_output_sequence_.load(std::memory_order_acquire);
              if (periodic_ack || final_ack)
                output_ack_sequence = completed_sequence;
            }
          }
        }
      }
    }
  }

  if (credit_mismatch) {
    this->request_transport_restart_();
    this->set_error_("output_credit_error", "Realtime output credit did not match buffered PCM");
    return;
  }
  if (output_ack_sequence != UINT32_MAX) {
    const uint64_t active_turn = this->get_active_turn_id_();
    if (active_turn != 0)
      this->acknowledge_output_through_(active_turn, output_ack_sequence);
  }

  if (this->speaker_pending_size_ > this->speaker_pending_offset_) {
    if (!this->tts_started_) {
      this->request_transport_restart_();
      this->set_error_("unexpected_audio", "Gateway sent output audio before tts.start");
      return;
    }
    const size_t remaining = this->speaker_pending_size_ - this->speaker_pending_offset_;
    const size_t written =
        this->speaker_->play(this->speaker_pending_.data() + this->speaker_pending_offset_, remaining, 0);
    if (written > remaining) {
      this->set_error_("speaker_write_error", "Speaker accepted an invalid byte count");
      return;
    }
    if (written > 0) {
      this->speaker_pending_offset_ += written;
      if (!this->replying_triggered_) {
        this->replying_triggered_ = true;
        this->set_state_(State::PLAYING);
        this->replying_trigger_.trigger();
      }
    }
  }

  const bool local_audio_empty =
      this->speaker_pending_offset_ >= this->speaker_pending_size_ && this->output_buffer_->available() == 0;
  if (!this->remote_turn_done_ || !this->tts_ended_)
    return;

  // The terminal controls establish a bounded drain phase even if the local
  // ring or speaker repeatedly makes no progress. Waiting until both are
  // empty before starting the timer would leave the most likely stuck path
  // governed only by the much longer whole-turn timeout.
  const uint32_t drain_now = millis();
  if (this->playback_drain_started_ms_ == 0)
    this->playback_drain_started_ms_ = drain_now == 0 ? 1 : drain_now;
  if (!local_audio_empty) {
    if (drain_now != 0 && drain_now - this->playback_drain_started_ms_ > PLAYBACK_DRAIN_TIMEOUT_MS)
      this->set_error_("playback_drain_timeout", "Timed out draining realtime response audio");
    return;
  }

  if (this->tts_started_) {
    if (!this->speaker_finish_requested_) {
      this->speaker_finish_requested_ = true;
      this->speaker_->finish();
    }
    if (this->speaker_->has_buffered_data() || !this->speaker_->is_stopped()) {
      if (drain_now != 0 && drain_now - this->playback_drain_started_ms_ > PLAYBACK_DRAIN_TIMEOUT_MS)
        this->set_error_("playback_drain_timeout", "Timed out draining realtime response audio");
      return;
    }
  }
  this->finish_turn_();
}

void HermesVoice::clear_output_audio_() {
  SemaphoreGuard lock(this->output_buffer_mutex_);
  if (lock.locked()) {
    if (this->output_buffer_ != nullptr)
      this->output_buffer_->reset();
    if (this->output_credit_queue_ != nullptr)
      xQueueReset(this->output_credit_queue_);
    this->output_credit_current_ = {};
    this->output_credit_current_valid_ = false;
    this->output_short_frame_seen_ = false;
  }
  this->speaker_pending_size_ = 0;
  this->speaker_pending_offset_ = 0;
}

void HermesVoice::loop() {
  if (!this->configured_)
    return;
  const uint32_t now = millis();
  const uint32_t stop_started = this->transport_stop_started_ms_.load(std::memory_order_acquire);
  if (stop_started != 0 && now != 0 && now - stop_started > TRANSPORT_STOP_WAIT_MS &&
      !this->transport_stop_reboot_requested_.exchange(true, std::memory_order_acq_rel)) {
    // ESP-IDF's stop waits without a caller-supplied deadline. If its owner
    // task wedges, keep the ESPHome loop alive long enough to request a clean
    // reboot rather than leaving voice permanently unavailable.
    ESP_LOGE(TAG, "Realtime transport stop exceeded its deadline; rebooting safely");
    App.safe_reboot();
    return;
  }
  const bool time_valid = this->time_source_ != nullptr && this->time_source_->now().is_valid();
  if (time_valid != this->tls_time_valid_.exchange(time_valid, std::memory_order_acq_rel)) {
    ESP_LOGI(TAG, "TLS clock is %s", time_valid ? "synchronized" : "not synchronized; transport paused");
    this->notify_sender_();
  }
  this->process_inbound_events_();

  const uint32_t ready_since = this->protocol_ready_since_ms_.load(std::memory_order_acquire);
  if (ready_since != 0 && this->protocol_ready_.load(std::memory_order_acquire) &&
      now - ready_since >= READY_STABLE_DURATION_MS) {
    this->protocol_ready_since_ms_.store(0, std::memory_order_release);
    this->reconnect_base_delay_ms_.store(RECONNECT_INITIAL_DELAY_MS, std::memory_order_release);
    ESP_LOGD(TAG, "Stable realtime epoch reset reconnect backoff");
  }

  if (this->conversation_reset_pending_.load(std::memory_order_acquire) &&
      this->protocol_ready_.load(std::memory_order_acquire)) {
    const uint32_t sent_at = this->conversation_reset_sent_ms_.load(std::memory_order_acquire);
    if (sent_at != 0 && now - sent_at >= RESET_ACK_TIMEOUT_MS) {
      const uint8_t attempts = this->conversation_reset_attempts_.load(std::memory_order_acquire);
      if (attempts < RESET_MAX_ATTEMPTS) {
        ESP_LOGW(TAG, "New-conversation acknowledgement timed out; retrying request (%u/%u)", attempts + 1,
                 RESET_MAX_ATTEMPTS);
        this->conversation_reset_sent_ms_.store(0, std::memory_order_release);
        this->conversation_reset_send_requested_.store(true, std::memory_order_release);
        this->notify_sender_();
      } else {
        ESP_LOGE(TAG, "New-conversation acknowledgement timed out after %u attempts", RESET_MAX_ATTEMPTS);
        this->conversation_reset_send_requested_.store(false, std::memory_order_release);
        this->conversation_reset_sent_ms_.store(0, std::memory_order_release);
        this->conversation_reset_attempts_.store(0, std::memory_order_release);
        this->set_conversation_reset_request_id_(0);
        this->conversation_reset_pending_.store(false, std::memory_order_release);
        this->request_transport_restart_();
        this->set_error_("conversation_reset_timeout", "Gateway did not acknowledge the new conversation");
        return;
      }
    }
  }

  if (this->ota_active_.load(std::memory_order_acquire))
    return;

  if (this->state_ == State::IDLE) {
    this->connection_lost_.store(false, std::memory_order_release);
    if (this->send_failed_.exchange(false, std::memory_order_acq_rel)) {
      ESP_LOGW(TAG, "Realtime gateway send failed while idle; replacing the transport epoch");
    }
    if (this->handshake_timeout_.exchange(false, std::memory_order_acq_rel)) {
      ESP_LOGW(TAG, "Realtime hello/ready handshake timed out; replacing the transport epoch");
    }
    if (this->protocol_error_.exchange(false, std::memory_order_acq_rel)) {
      ESP_LOGW(TAG, "Malformed realtime data while idle; starting a fresh transport epoch");
      this->protocol_ready_.store(false, std::memory_order_release);
      this->request_transport_restart_();
    }
  } else if (this->state_ != State::ERROR && this->state_ != State::STOPPING_MICROPHONE) {
    if (this->connection_lost_.exchange(false, std::memory_order_acq_rel)) {
      this->set_error_("gateway_disconnected", "Realtime gateway disconnected during the turn");
      return;
    }
    if (this->send_failed_.exchange(false, std::memory_order_acq_rel)) {
      this->set_error_("gateway_send_failed", "Could not send realtime voice data");
      return;
    }
    if (this->handshake_timeout_.exchange(false, std::memory_order_acq_rel)) {
      this->set_error_("gateway_handshake_timeout", "Gateway did not complete the realtime handshake");
      return;
    }
    if (this->protocol_error_.exchange(false, std::memory_order_acq_rel)) {
      this->request_transport_restart_();
      this->set_error_("realtime_protocol_error", "Realtime gateway sent invalid protocol data");
      return;
    }
    if (this->input_buffer_overflow_.load(std::memory_order_acquire)) {
      this->set_error_("input_buffer_overflow", "Realtime microphone buffer overflowed");
      return;
    }
    if (this->output_buffer_overflow_.load(std::memory_order_acquire)) {
      this->request_transport_restart_();
      this->set_error_("output_buffer_overflow", "Realtime playback buffer overflowed");
      return;
    }
    if (this->turn_started_ms_ != 0 && now - this->turn_started_ms_ >= this->request_timeout_ms_) {
      this->set_error_("gateway_timeout", "Realtime voice turn exceeded its overall timeout");
      return;
    }
  }

  switch (this->state_) {
    case State::IDLE:
      break;

    case State::WAITING_FOR_WAKE_WORD_STOP:
      if (this->micro_wake_word_ == nullptr || !this->micro_wake_word_->is_running()) {
        this->begin_capture_();
      } else if (now - this->state_started_ms_ > COMPONENT_TRANSITION_TIMEOUT_MS) {
        this->set_error_("wake_word_stop_timeout", "Wake-word task did not stop");
      }
      break;

    case State::STARTING_MICROPHONE:
      if (this->microphone_source_->is_running()) {
        this->capturing_.store(true, std::memory_order_release);
        this->capture_started_ms_ = now;
        this->set_state_(State::LISTENING);
        this->listening_trigger_.trigger();
      } else if (now - this->state_started_ms_ > COMPONENT_TRANSITION_TIMEOUT_MS) {
        this->set_error_("microphone_start_timeout", "Microphone did not start");
      }
      break;

    case State::LISTENING: {
      if (!network::is_connected() || !this->websocket_connected_.load(std::memory_order_acquire)) {
        this->set_error_("network_lost", "Network disconnected while streaming microphone audio");
        break;
      }
      const bool speech_seen = this->speech_seen_.load(std::memory_order_acquire);
      if (speech_seen && !this->speech_start_triggered_) {
        this->speech_start_triggered_ = true;
        this->speech_start_trigger_.trigger();
      }
      const uint32_t elapsed = now - this->capture_started_ms_;
      if (elapsed >= this->max_recording_duration_ms_) {
        if (speech_seen)
          this->commit_turn_();
        else
          this->set_error_("no_speech", "No speech was detected");
      } else if (!speech_seen && elapsed >= this->speech_timeout_ms_) {
        this->set_error_("no_speech", "No speech was detected");
      } else if (speech_seen && elapsed >= this->min_recording_duration_ms_) {
        const uint32_t last_voice = this->last_voice_ms_.load(std::memory_order_acquire);
        if (last_voice != 0 && now - last_voice >= this->silence_duration_ms_)
          this->commit_turn_();
      }
      break;
    }

    case State::PROCESSING:
    case State::PLAYING:
      if (this->commit_sent_.load(std::memory_order_acquire) &&
          !this->barge_monitoring_.exchange(true, std::memory_order_acq_rel) && this->micro_wake_word_ != nullptr) {
        // The I2S microphone is listener-counted, so wake inference can share
        // the live source with channel-0 AEC capture during response playback.
        this->micro_wake_word_->start();
      }
      if (this->barge_requested_.load(std::memory_order_acquire)) {
        this->begin_barge_in_();
        break;
      }
      this->service_playback_();
      break;

    case State::STOPPING_MICROPHONE:
      if (this->microphone_source_->is_stopped()) {
        this->return_to_idle_();
      } else if (now - this->state_started_ms_ > COMPONENT_TRANSITION_TIMEOUT_MS) {
        this->set_error_("microphone_stop_timeout", "Microphone did not stop");
      }
      break;

    case State::ERROR:
      if (now - this->state_started_ms_ >= 1000 && this->microphone_source_->is_stopped())
        this->return_to_idle_();
      break;
  }
}

}  // namespace esphome::hermes_voice

#endif  // USE_ESP32
