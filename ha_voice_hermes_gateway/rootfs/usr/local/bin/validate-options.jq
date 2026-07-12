def value_or($source; $name; $default):
  if ($source | has($name)) then $source[$name] else $default end;

def bytes_between($minimum; $maximum):
  type == "string" and
  (utf8bytelength >= $minimum) and
  (utf8bytelength <= $maximum);

def no_controls:
  type == "string" and all(explode[]; . >= 32 and . != 127);

def header_secret($maximum):
  bytes_between(1; $maximum) and
  all(explode[]; . >= 33 and . <= 126);

def identifier($maximum):
  bytes_between(1; $maximum) and test("^[A-Za-z0-9._/:@-]+$");

def safe_certificate_name:
  bytes_between(1; 255) and
  test("^[A-Za-z0-9][A-Za-z0-9._-]*$") and
  (contains("..") | not);

def https_url:
  bytes_between(9; 2048) and
  test("^https://(?:[A-Za-z0-9.-]+|\\[[0-9A-Fa-f:]+\\])(?::[0-9]{1,5})?(?:/[!$&'()*+,;=:@A-Za-z0-9._~%/-]*)?$");

def integer_between($minimum; $maximum):
  type == "number" and floor == . and . >= $minimum and . <= $maximum;

def valid_device:
  type == "object" and
  ((keys_unsorted - ["device_id", "device_token", "hermes_session_key"]) | length == 0) and
  (.device_id | bytes_between(1; 64) and test("^[A-Za-z0-9][A-Za-z0-9._-]*$")) and
  (.device_token | type == "string" and test("^[!-~]{32,512}$")) and
  (.hermes_session_key | header_secret(256));

def valid_instructions:
  type == "string" and
  utf8bytelength <= 2048 and
  all(explode[]; (. >= 32 and . != 127) or . == 9 or . == 10 or . == 13);

def require($condition; $message):
  if $condition then . else error($message) end;

def allowed_keys:
  [
    "certfile",
    "keyfile",
    "allow_private_upstreams",
    "certificate_poll_seconds",
    "hermes_base_url",
    "hermes_api_key",
    "hermes_model",
    "hermes_profile_id",
    "hermes_binding_revision",
    "hermes_voice_instructions",
    "cf_access_client_id",
    "cf_access_client_secret",
    "elevenlabs_api_key",
    "elevenlabs_voice_id",
    "realtime_stt_model",
    "tts_model",
    "elevenlabs_enable_logging",
    "stt_language_code",
    "conversation_idle_seconds",
    "realtime_max_audio_seconds",
    "realtime_max_output_seconds",
    "max_connection_audio_seconds",
    "max_turns_per_connection",
    "max_messages_per_connection",
    "diagnostic_v1_enabled",
    "devices"
  ];

if type != "object" then
  error("options must be a JSON object")
else
  .
end
| . as $source
| require((($source | keys_unsorted) - allowed_keys | length) == 0;
          "options contain an unsupported key")
| {
    certfile: value_or($source; "certfile"; "fullchain.pem"),
    keyfile: value_or($source; "keyfile"; "privkey.pem"),
    allow_private_upstreams: value_or($source; "allow_private_upstreams"; false),
    certificate_poll_seconds: value_or($source; "certificate_poll_seconds"; 60),
    hermes_base_url: value_or($source; "hermes_base_url"; null),
    hermes_api_key: value_or($source; "hermes_api_key"; null),
    hermes_model: value_or($source; "hermes_model"; "hermes-agent"),
    hermes_profile_id: value_or($source; "hermes_profile_id"; "voice-safe"),
    hermes_binding_revision: value_or($source; "hermes_binding_revision"; "v1"),
    hermes_voice_instructions: value_or($source; "hermes_voice_instructions"; ""),
    cf_access_client_id: value_or($source; "cf_access_client_id"; ""),
    cf_access_client_secret: value_or($source; "cf_access_client_secret"; ""),
    elevenlabs_api_key: value_or($source; "elevenlabs_api_key"; null),
    elevenlabs_voice_id: value_or($source; "elevenlabs_voice_id"; null),
    realtime_stt_model: value_or($source; "realtime_stt_model"; "scribe_v2_realtime"),
    tts_model: value_or($source; "tts_model"; "eleven_flash_v2_5"),
    elevenlabs_enable_logging: value_or($source; "elevenlabs_enable_logging"; true),
    stt_language_code: value_or($source; "stt_language_code"; ""),
    conversation_idle_seconds: value_or($source; "conversation_idle_seconds"; 900),
    realtime_max_audio_seconds: value_or($source; "realtime_max_audio_seconds"; 30),
    realtime_max_output_seconds: value_or($source; "realtime_max_output_seconds"; 30),
    max_connection_audio_seconds: value_or($source; "max_connection_audio_seconds"; 900),
    max_turns_per_connection: value_or($source; "max_turns_per_connection"; 256),
    max_messages_per_connection: value_or($source; "max_messages_per_connection"; 16384),
    diagnostic_v1_enabled: value_or($source; "diagnostic_v1_enabled"; false),
    devices: value_or($source; "devices"; null)
  }
| require((.certfile | safe_certificate_name); "certfile is invalid")
| require((.keyfile | safe_certificate_name); "keyfile is invalid")
| require((.certfile != .keyfile); "certfile and keyfile must be different files")
| require((.allow_private_upstreams | type == "boolean");
          "allow_private_upstreams must be boolean")
| require((.certificate_poll_seconds | integer_between(5; 3600));
          "certificate_poll_seconds must be between 5 and 3600")
| require((.hermes_base_url | https_url); "hermes_base_url must be a supported HTTPS URL")
| require((.hermes_api_key | header_secret(4096)); "hermes_api_key is invalid")
| require((.hermes_model | identifier(160)); "hermes_model is invalid")
| require((.hermes_profile_id | identifier(160)); "hermes_profile_id is invalid")
| require((.hermes_binding_revision | identifier(160)); "hermes_binding_revision is invalid")
| require((.hermes_voice_instructions | valid_instructions);
          "hermes_voice_instructions is invalid")
| require((.cf_access_client_id == "" or (.cf_access_client_id | header_secret(4096)));
          "cf_access_client_id is invalid")
| require((.cf_access_client_secret == "" or (.cf_access_client_secret | header_secret(4096)));
          "cf_access_client_secret is invalid")
| require(((.cf_access_client_id == "") == (.cf_access_client_secret == ""));
          "Cloudflare Access credentials must be configured together")
| require((.elevenlabs_api_key | header_secret(4096)); "elevenlabs_api_key is invalid")
| require((.elevenlabs_voice_id | identifier(128)); "elevenlabs_voice_id is invalid")
| require((.realtime_stt_model | identifier(160)); "realtime_stt_model is invalid")
| require((.tts_model | identifier(160)); "tts_model is invalid")
| require((.elevenlabs_enable_logging | type == "boolean");
          "elevenlabs_enable_logging must be boolean")
| require((.stt_language_code == "" or
           (.stt_language_code | type == "string" and test("^[A-Za-z-]{1,16}$")));
          "stt_language_code is invalid")
| require((.conversation_idle_seconds | integer_between(60; 31536000));
          "conversation_idle_seconds must be between 60 and 31536000")
| require((.realtime_max_audio_seconds | integer_between(1; 30));
          "realtime_max_audio_seconds must be between 1 and 30")
| require((.realtime_max_output_seconds | integer_between(1; 30));
          "realtime_max_output_seconds must be between 1 and 30")
| .realtime_max_audio_seconds as $minimum_connection_audio
| require((.max_connection_audio_seconds | integer_between($minimum_connection_audio; 86400));
          "max_connection_audio_seconds is invalid")
| require((.max_turns_per_connection | integer_between(1; 10000));
          "max_turns_per_connection is invalid")
| require((.max_messages_per_connection | integer_between(128; 1000000));
          "max_messages_per_connection is invalid")
| require((.diagnostic_v1_enabled | type == "boolean");
          "diagnostic_v1_enabled must be boolean")
| require((.devices | type == "array" and length >= 1 and length <= 128 and all(valid_device));
          "devices must contain between 1 and 128 valid device records")
| . as $normalized
| ($normalized.devices | length) as $device_count
| require(([$normalized.devices[].device_id] | unique | length) == $device_count;
          "device_id values must be unique")
| require(([$normalized.devices[].device_token] | unique | length) == $device_count;
          "device_token values must be unique")
