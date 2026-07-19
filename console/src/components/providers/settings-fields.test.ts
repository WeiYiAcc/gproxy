import { describe, expect, it } from "vitest";

import { assembleSettings, initSettingsState } from "./settings-fields";

describe("custom provider settings", () => {
  it("defaults to bearer with raw and prefetch disabled", () => {
    const state = initSettingsState({ base_url: "https://custom.example" });
    expect(state.apiKeyHeader).toBe("bearer");
    expect(state.preserveRawRequestBody).toBe(false);
    expect(state.prefetchStreamBeforeCommit).toBe(false);
  });

  it("assembles all custom compatibility settings and preserves unknown fields", () => {
    const state = initSettingsState({
      base_url: "https://custom.example",
      api_key_header: "x-api-key",
      preserve_raw_request_body: true,
      prefetch_stream_before_commit: true,
    });
    expect(
      assembleSettings({ vendor_option: "kept" }, state, "custom"),
    ).toMatchObject({
      base_url: "https://custom.example",
      api_key_header: "x-api-key",
      preserve_raw_request_body: true,
      prefetch_stream_before_commit: true,
      vendor_option: "kept",
    });
  });

  it("does not save custom-only settings for another channel", () => {
    const state = initSettingsState({
      api_key_header: "x-goog-api-key",
      preserve_raw_request_body: true,
      prefetch_stream_before_commit: true,
    });
    const result = assembleSettings({}, state, "openai");
    expect(result).not.toHaveProperty("api_key_header");
    expect(result).not.toHaveProperty("preserve_raw_request_body");
    expect(result).not.toHaveProperty("prefetch_stream_before_commit");
  });
});
