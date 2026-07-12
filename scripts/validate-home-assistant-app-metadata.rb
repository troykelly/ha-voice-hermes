#!/usr/bin/env ruby
# frozen_string_literal: true

require "json"
require "yaml"

ROOT = File.expand_path("..", __dir__)
APP = File.join(ROOT, "ha_voice_hermes_gateway")

def fail_check(message)
  warn("Home Assistant App metadata validation failed: #{message}")
  exit(1)
end

def load_mapping(path)
  value = YAML.safe_load(File.read(path, encoding: "UTF-8"), aliases: false)
  fail_check("#{path} is not a YAML mapping") unless value.is_a?(Hash)
  value
rescue Psych::Exception => error
  fail_check("#{path} is invalid YAML: #{error.message}")
end

repository = load_mapping(File.join(ROOT, "repository.yaml"))
config = load_mapping(File.join(APP, "config.yaml"))
translation = load_mapping(File.join(APP, "translations", "en.yaml"))
package = JSON.parse(File.read(File.join(APP, "package.json"), encoding: "UTF-8"))
package_lock = JSON.parse(File.read(File.join(APP, "package-lock.json"), encoding: "UTF-8"))

fail_check("repository name is missing") unless repository["name"].is_a?(String) && !repository["name"].empty?
fail_check("repository URL must be the canonical HTTPS GitHub URL") unless repository["url"] == "https://github.com/troykelly/ha-voice-hermes"
fail_check("repository maintainer is missing") unless repository["maintainer"].is_a?(String) && !repository["maintainer"].empty?

fail_check("generic multi-architecture image name changed") unless config["image"] == "ghcr.io/troykelly/ha-voice-hermes-gateway"
fail_check("supported architectures changed") unless config["arch"].sort == %w[aarch64 amd64]
fail_check("App version and runtime package version differ") unless config["version"] == package["version"]
fail_check("runtime package and lockfile versions differ") unless package["version"] == package_lock["version"] && package["version"] == package_lock.dig("packages", "", "version")
fail_check("App version is not stable semantic versioning") unless config["version"].is_a?(String) && config["version"].match?(/\A(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\z/)
fail_check("cold backup is required") unless config["backup"] == "cold"
fail_check("experimental stage is required before physical release gates pass") unless config["stage"] == "experimental"
fail_check("/tmp must remain tmpfs-backed") unless config["tmpfs"] == true
fail_check("the App must use S6 init without Supervisor init emulation") unless config["init"] == false
fail_check("a custom AppArmor profile is required") unless config.fetch("apparmor", true) == true
%w[host_network privileged full_access hassio_api homeassistant_api docker_api].each do |capability|
  fail_check("#{capability} must not be enabled") if config[capability] == true
end
fail_check("only the read-only ssl map is allowed") unless config["map"] == [{ "type" => "ssl", "read_only" => true }]
fail_check("only the TLS gateway port may be published") unless config["ports"] == { "8443/tcp" => 8443 }

options = config["options"]
schema = config["schema"]
fail_check("options is not a mapping") unless options.is_a?(Hash)
fail_check("schema is not a mapping") unless schema.is_a?(Hash)
unless (options.keys - schema.keys).empty?
  fail_check("every default option must have a schema entry")
end

translated = translation["configuration"]
fail_check("translation configuration is not a mapping") unless translated.is_a?(Hash)
unless schema.keys.sort == translated.keys.sort
  fail_check("top-level translation keys do not exactly match schema keys")
end

device_schema = schema["devices"]
device_translation = translated.dig("devices", "fields")
unless device_schema.is_a?(Array) && device_schema.one? && device_schema.first.is_a?(Hash)
  fail_check("devices schema must contain exactly one record shape")
end
unless device_translation.is_a?(Hash) && device_schema.first.keys.sort == device_translation.keys.sort
  fail_check("device field translation keys do not exactly match the nested schema")
end

ports = config["ports"]
network = translation["network"]
unless ports.is_a?(Hash) && network.is_a?(Hash) && ports.keys.sort == network.keys.sort
  fail_check("network translation keys do not exactly match published ports")
end

%w[README.md DOCS.md CHANGELOG.md apparmor.txt].each do |name|
  path = File.join(APP, name)
  fail_check("#{name} is missing or empty") unless File.file?(path) && File.size(path).positive?
end
unless File.read(File.join(APP, "CHANGELOG.md"), encoding: "UTF-8").match?(/^## #{Regexp.escape(config["version"])}$/)
  fail_check("CHANGELOG.md has no heading for the configured App version")
end

puts("Home Assistant App repository, translation, and documentation metadata are consistent")
