// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, waitFor } from "@testing-library/react";
import React from "react";

/**
 * `TempsAnalyticsProvider` used to pass an allow-list of eleven
 * `sessionRecordingConfig` fields to the recorder and silently drop the rest,
 * so the provider path behaved differently from a standalone
 * `<SessionRecorder>` given the same options. The core recorder is replaced
 * with a spy here so the test can read the exact options it was built with.
 */

const recorderOptions: Array<Record<string, unknown>> = [];

vi.mock("@temps-sdk/analytics-core", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@temps-sdk/analytics-core")>();
  class SpySessionRecorder {
    constructor(options: Record<string, unknown>) {
      recorderOptions.push(options);
    }
    destroy(): void {}
  }
  return { ...actual, SessionRecorder: SpySessionRecorder };
});

import { TempsAnalyticsProvider } from "../Provider";
import type { SessionRecorderConfig } from "../SessionRecorder";

/** Every field, each set to a value that differs from its default. */
const fullConfig: Required<SessionRecorderConfig> = {
  excludedPaths: ["/private/*"],
  useDefaultExcludedPaths: false,
  sessionSampleRate: 0.5,
  maskAllInputs: false,
  maskTextSelector: ".mask-me",
  blockClass: "block-me",
  ignoreClass: "ignore-me",
  maskTextClass: "mask-text",
  recordCanvas: true,
  collectFonts: false,
  batchSize: 25,
  flushInterval: 2500,
  idleTimeout: 0,
  pauseOnHidden: false,
  checkoutEveryNms: 120000,
  checkoutEveryNth: 1000,
  maxBufferedEvents: 250,
  debug: true,
  slimDOMOptions: { script: true },
  maskInputOptions: { password: true, email: false },
  ignoreSelector: ".ignore-selector",
  blockSelector: ".block-selector",
  sampling: { scroll: 1000, mousemove: false, input: "all" },
};

describe("TempsAnalyticsProvider sessionRecordingConfig", () => {
  beforeEach(() => {
    recorderOptions.length = 0;
    // Real storage answers null for a missing key; the shared mock answers
    // undefined, which the recording toggle reads as "disabled".
    vi.mocked(window.localStorage.getItem).mockReturnValue(null);
  });

  it("forwards every config field to the recorder", async () => {
    render(
      <TempsAnalyticsProvider
        basePath="/custom/_temps"
        ingestKey="pa_test"
        domain="example.com"
        enableSessionRecording={true}
        sessionRecordingConfig={fullConfig}
      >
        <div>app</div>
      </TempsAnalyticsProvider>,
    );

    await waitFor(() => expect(recorderOptions).toHaveLength(1));
    expect(recorderOptions[0]).toMatchObject({
      ...fullConfig,
      basePath: "/custom/_temps",
      ingestKey: "pa_test",
      domain: "example.com",
      enabled: true,
    });
  });

  it("does not let config keys override the provider's transport props", async () => {
    const config = {
      basePath: "https://elsewhere.invalid/_temps",
      ingestKey: "pa_other",
    } as unknown as SessionRecorderConfig;

    render(
      <TempsAnalyticsProvider
        basePath="/api/_temps"
        enableSessionRecording={true}
        sessionRecordingConfig={config}
      >
        <div>app</div>
      </TempsAnalyticsProvider>,
    );

    await waitFor(() => expect(recorderOptions).toHaveLength(1));
    expect(recorderOptions[0].basePath).toBe("/api/_temps");
    expect(recorderOptions[0].ingestKey).toBeUndefined();
  });
});
