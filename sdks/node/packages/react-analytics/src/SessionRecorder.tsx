// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

"use client";
import { useEffect, useMemo, useRef } from "react";
import {
  SessionRecorder as CoreSessionRecorder,
  type SessionRecordingConfig,
} from "@temps-sdk/analytics-core";

export const SESSION_RECORDER_ENDPOINT = "session-replay";

/**
 * Everything that tunes the recorder: the shared `SessionRecordingConfig`
 * (documented there, defaults included) plus the lower-level rrweb options the
 * core recorder also accepts. `TempsAnalyticsProvider`'s
 * `sessionRecordingConfig` takes exactly this shape, so the provider path and a
 * standalone `SessionRecorder` accept the same options.
 */
export interface SessionRecorderConfig extends SessionRecordingConfig {
  slimDOMOptions?: Record<string, boolean>;
  maskInputOptions?: {
    password?: boolean;
    email?: boolean;
  };
  /** CSS selector for elements whose events are ignored. Defaults to "[data-ignore]". */
  ignoreSelector?: string;
  /** CSS selector for elements blocked from recording. Defaults to "[data-private]". */
  blockSelector?: string;
  sampling?: {
    scroll?: number;
    media?: number;
    mouseInteraction?: boolean | {
      click?: boolean;
      dblclick?: boolean;
      contextmenu?: boolean;
      focus?: boolean;
      blur?: boolean;
      touchstart?: boolean;
      touchend?: boolean;
      touchcancel?: boolean;
      play?: boolean;
      pause?: boolean;
    };
    mousemove?: boolean | number;
    input?: "all" | "last";
    canvas?: number | "all";
  };
}

export interface SessionRecorderProps extends SessionRecorderConfig {
  basePath: string;  // Required, no default
  /**
   * Analytics ingest key (`pa_…`). Required only when Temps does not serve or
   * proxy the app. See `AnalyticsClientOptions.ingestKey`.
   */
  ingestKey?: string;
  domain?: string;
  enabled?: boolean;
}

/**
 * React binding for the framework-agnostic recorder in `@temps-sdk/analytics-core`.
 *
 * This used to be a second, independent rrweb implementation. Keeping two
 * copies meant every ingest fix had to be made twice and they drifted instead
 * — batch handling, the start-up race, and idle gating were all fixed in one
 * and not the other. The component now owns only React lifecycle; all
 * recording behaviour lives in the core class.
 */
export function SessionRecorder(props: SessionRecorderProps): null {
  const recorderRef = useRef<CoreSessionRecorder | null>(null);

  // Object and array props are rebuilt on every render by the caller (and by
  // this component's own defaults), so depending on them directly would tear
  // down and re-create the recorder on each render — losing the session and
  // re-snapshotting the DOM every time. Key the effect on the serialized
  // options instead.
  const optionsKey = useMemo(() => JSON.stringify(props), [props]);

  useEffect(() => {
    if (!props.enabled) return;

    // Forward every option rather than an allow-list: a hand-maintained list
    // is how options documented on the config silently stopped reaching the
    // recorder.
    const { sampling, ...options } = props;
    const recorder = new CoreSessionRecorder({
      ...options,
      enabled: true,
      sampling: sampling as Record<string, unknown> | undefined,
    });
    recorderRef.current = recorder;

    return () => {
      recorder.destroy();
      recorderRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [optionsKey]);

  return null;
}
