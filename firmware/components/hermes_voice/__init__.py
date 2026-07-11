# SPDX-License-Identifier: MIT
"""Realtime Hermes voice client for the Home Assistant Voice PE.

The component keeps an authenticated WSS connection open, streams PCM16/16 kHz
microphone frames as they are captured, and plays bounded PCM response frames as
they arrive. Provider credentials remain in the gateway.
"""

import re
from urllib.parse import urlsplit

from esphome import automation
from esphome.automation import register_action, register_condition
import esphome.codegen as cg
from esphome.components import (
    esp32,
    microphone,
    micro_wake_word,
    network,
    psram,
    speaker,
    wifi,
)
import esphome.config_validation as cv
from esphome.const import CONF_ID, CONF_MICROPHONE, CONF_ON_ERROR, CONF_SPEAKER


CODEOWNERS = []
DEPENDENCIES = ["network", "microphone", "speaker", "psram"]
AUTO_LOAD = ["audio", "json", "ring_buffer"]

CONF_GATEWAY_URL = "gateway_url"
CONF_AUTH_TOKEN = "auth_token"
CONF_DEVICE_ID = "device_id"
CONF_MICRO_WAKE_WORD = "micro_wake_word"
CONF_SILENCE_THRESHOLD = "silence_threshold"
CONF_SILENCE_DURATION = "silence_duration"
CONF_SPEECH_TIMEOUT = "speech_timeout"
CONF_MAX_RECORDING_DURATION = "max_recording_duration"
CONF_MIN_RECORDING_DURATION = "min_recording_duration"
CONF_REQUEST_TIMEOUT = "request_timeout"
CONF_OUTPUT_SAMPLE_RATE = "output_sample_rate"
CONF_TASK_STACK_IN_PSRAM = "task_stack_in_psram"

CONF_ON_LISTENING = "on_listening"
CONF_ON_SPEECH_START = "on_speech_start"
CONF_ON_THINKING = "on_thinking"
CONF_ON_REPLYING = "on_replying"
CONF_ON_IDLE = "on_idle"


def _wss_url(value):
    value = cv.string_strict(value)
    if not value:
        return value
    if not value.startswith("wss://"):
        raise cv.Invalid("gateway_url must use wss://")
    # Reuse ESPHome's mature URL validation by validating the equivalent HTTPS
    # URL. Keep the original WSS scheme for generated firmware.
    cv.url("https://" + value[len("wss://") :])
    parsed = urlsplit(value)
    if parsed.username is not None or parsed.password is not None:
        raise cv.Invalid("gateway_url must not contain user information")
    if parsed.query or parsed.fragment:
        raise cv.Invalid("gateway_url must not contain a query or fragment")
    return value


def _auth_token(value):
    value = cv.string_strict(value)
    if not value:
        return value
    encoded = value.encode("utf-8")
    if not 16 <= len(encoded) <= 512 or any(
        byte < 0x20 or byte == 0x7F for byte in encoded
    ):
        raise cv.Invalid(
            "auth_token must contain 16-512 bytes without control characters"
        )
    return value


def _device_id(value):
    value = cv.string_strict(value)
    if not value:
        return value
    if re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}", value) is None:
        raise cv.Invalid(
            "device_id must be 1-64 ASCII letters, numbers, '.', '_', or '-', and start alphanumeric"
        )
    return value


def _validate_gateway_pair(config):
    has_url = bool(config[CONF_GATEWAY_URL])
    has_token = bool(config[CONF_AUTH_TOKEN])
    if has_url != has_token:
        raise cv.Invalid(
            "gateway_url and auth_token must either both be configured or both be empty"
        )
    return config


def _request_realtime_networking(config):
    network.require_high_performance_networking()
    wifi.enable_runtime_power_save_control()
    return config


hermes_voice_ns = cg.esphome_ns.namespace("hermes_voice")
HermesVoice = hermes_voice_ns.class_("HermesVoice", cg.Component)
StartAction = hermes_voice_ns.class_(
    "StartAction", automation.Action, cg.Parented.template(HermesVoice)
)
StopAction = hermes_voice_ns.class_(
    "StopAction", automation.Action, cg.Parented.template(HermesVoice)
)
NewConversationAction = hermes_voice_ns.class_(
    "NewConversationAction", automation.Action, cg.Parented.template(HermesVoice)
)
IsRunningCondition = hermes_voice_ns.class_(
    "IsRunningCondition", automation.Condition, cg.Parented.template(HermesVoice)
)


CONFIG_SCHEMA = cv.All(
    cv.Schema(
        {
            cv.GenerateID(): cv.declare_id(HermesVoice),
            # Empty values are an intentional factory/adoption state. ESPHome
            # API, OTA, media and provisioning remain available until the
            # adopter adds both gateway values and installs an OTA update.
            cv.Optional(CONF_AUTH_TOKEN, default=""): cv.sensitive(_auth_token),
            cv.Optional(CONF_GATEWAY_URL, default=""): _wss_url,
            cv.Optional(CONF_DEVICE_ID): _device_id,
            cv.Required(CONF_MICROPHONE): microphone.microphone_source_schema(
                min_bits_per_sample=16,
                max_bits_per_sample=16,
                min_channels=1,
                max_channels=1,
            ),
            cv.Required(CONF_SPEAKER): cv.use_id(speaker.Speaker),
            cv.Optional(CONF_MICRO_WAKE_WORD): cv.use_id(
                micro_wake_word.MicroWakeWord
            ),
            cv.Optional(CONF_SILENCE_THRESHOLD, default=250): cv.int_range(
                min=1, max=32767
            ),
            cv.Optional(CONF_SILENCE_DURATION, default="500ms"): cv.All(
                cv.positive_time_period_milliseconds,
                cv.Range(
                    min=cv.TimePeriod(milliseconds=200),
                    max=cv.TimePeriod(seconds=5),
                ),
            ),
            cv.Optional(CONF_SPEECH_TIMEOUT, default="8s"): cv.All(
                cv.positive_time_period_milliseconds,
                cv.Range(
                    min=cv.TimePeriod(seconds=1),
                    max=cv.TimePeriod(seconds=30),
                ),
            ),
            cv.Optional(CONF_MAX_RECORDING_DURATION, default="30s"): cv.All(
                cv.positive_time_period_milliseconds,
                cv.Range(
                    min=cv.TimePeriod(seconds=2),
                    max=cv.TimePeriod(seconds=60),
                ),
            ),
            cv.Optional(CONF_MIN_RECORDING_DURATION, default="300ms"): cv.All(
                cv.positive_time_period_milliseconds,
                cv.Range(
                    min=cv.TimePeriod(milliseconds=100),
                    max=cv.TimePeriod(seconds=3),
                ),
            ),
            cv.Optional(CONF_REQUEST_TIMEOUT, default="5min"): cv.All(
                cv.positive_time_period_milliseconds,
                cv.Range(
                    min=cv.TimePeriod(seconds=10),
                    max=cv.TimePeriod(minutes=15),
                ),
            ),
            # Realtime protocol v2 is fixed to signed PCM16 mono at 16 kHz.
            cv.Optional(CONF_OUTPUT_SAMPLE_RATE, default=16000): cv.int_range(
                min=16000, max=16000
            ),
            cv.Optional(
                CONF_TASK_STACK_IN_PSRAM, default=False
            ): psram.validate_task_stack_in_psram,
            cv.Optional(CONF_ON_LISTENING): automation.validate_automation(
                single=True
            ),
            cv.Optional(CONF_ON_SPEECH_START): automation.validate_automation(
                single=True
            ),
            cv.Optional(CONF_ON_THINKING): automation.validate_automation(single=True),
            cv.Optional(CONF_ON_REPLYING): automation.validate_automation(single=True),
            cv.Optional(CONF_ON_IDLE): automation.validate_automation(single=True),
            cv.Optional(CONF_ON_ERROR): automation.validate_automation(single=True),
        }
    ).extend(cv.COMPONENT_SCHEMA),
    _validate_gateway_pair,
    _request_realtime_networking,
)


FINAL_VALIDATE_SCHEMA = cv.Schema(
    {
        cv.Required(CONF_MICROPHONE): microphone.final_validate_microphone_source_schema(
            "hermes_voice", sample_rate=16000
        ),
    },
    extra=cv.ALLOW_EXTRA,
)


async def to_code(config):
    var = cg.new_Pvariable(config[CONF_ID])
    await cg.register_component(var, config)

    mic_source = await microphone.microphone_source_to_code(config[CONF_MICROPHONE])
    cg.add(var.set_microphone_source(mic_source))

    output = await cg.get_variable(config[CONF_SPEAKER])
    cg.add(var.set_speaker(output))

    if CONF_MICRO_WAKE_WORD in config:
        mww = await cg.get_variable(config[CONF_MICRO_WAKE_WORD])
        cg.add(var.set_micro_wake_word(mww))

    cg.add(var.set_gateway_url(config[CONF_GATEWAY_URL]))
    cg.add(var.set_auth_token(config[CONF_AUTH_TOKEN]))
    if CONF_DEVICE_ID in config and config[CONF_DEVICE_ID]:
        cg.add(var.set_device_id(config[CONF_DEVICE_ID]))
    cg.add(var.set_silence_threshold(config[CONF_SILENCE_THRESHOLD]))
    cg.add(
        var.set_silence_duration_ms(
            config[CONF_SILENCE_DURATION].total_milliseconds
        )
    )
    cg.add(var.set_speech_timeout_ms(config[CONF_SPEECH_TIMEOUT].total_milliseconds))
    cg.add(
        var.set_max_recording_duration_ms(
            config[CONF_MAX_RECORDING_DURATION].total_milliseconds
        )
    )
    cg.add(
        var.set_min_recording_duration_ms(
            config[CONF_MIN_RECORDING_DURATION].total_milliseconds
        )
    )
    cg.add(var.set_request_timeout_ms(config[CONF_REQUEST_TIMEOUT].total_milliseconds))
    cg.add(var.set_output_sample_rate(config[CONF_OUTPUT_SAMPLE_RATE]))
    cg.add(var.set_task_stack_in_psram(config[CONF_TASK_STACK_IN_PSRAM]))
    if config[CONF_TASK_STACK_IN_PSRAM]:
        psram.request_external_task_stack()

    trigger_configs = (
        (CONF_ON_LISTENING, var.get_listening_trigger(), []),
        (CONF_ON_SPEECH_START, var.get_speech_start_trigger(), []),
        (CONF_ON_THINKING, var.get_thinking_trigger(), []),
        (CONF_ON_REPLYING, var.get_replying_trigger(), []),
        (CONF_ON_IDLE, var.get_idle_trigger(), []),
        (
            CONF_ON_ERROR,
            var.get_error_trigger(),
            [(cg.std_string, "code"), (cg.std_string, "message")],
        ),
    )
    for key, trigger, args in trigger_configs:
        if key in config:
            await automation.build_automation(trigger, args, config[key])

    esp32.add_idf_component(name="espressif/esp_websocket_client", ref="1.7.0")
    esp32.add_idf_sdkconfig_option("CONFIG_MBEDTLS_CERTIFICATE_BUNDLE", True)


HERMES_VOICE_ACTION_SCHEMA = cv.Schema({cv.GenerateID(): cv.use_id(HermesVoice)})


@register_action(
    "hermes_voice.start", StartAction, HERMES_VOICE_ACTION_SCHEMA, synchronous=True
)
@register_action(
    "hermes_voice.stop", StopAction, HERMES_VOICE_ACTION_SCHEMA, synchronous=True
)
@register_action(
    "hermes_voice.new_conversation",
    NewConversationAction,
    HERMES_VOICE_ACTION_SCHEMA,
    synchronous=True,
)
@register_condition(
    "hermes_voice.is_running", IsRunningCondition, HERMES_VOICE_ACTION_SCHEMA
)
async def hermes_voice_action_to_code(config, action_id, template_arg, args):
    var = cg.new_Pvariable(action_id, template_arg)
    await cg.register_parented(var, config[CONF_ID])
    return var
