// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, renderHook, waitFor } from "@testing-library/react";
import React from "react";
import { TempsAnalyticsProvider, useTempsAnalytics } from "../Provider";
import { DEFAULT_BASE_PATH } from "./test-constants";

describe("TempsAnalyticsProvider", () => {
  beforeEach(() => {
    vi.clearAllMocks();

    // Real storage answers null for a missing key; the shared mock answers
    // undefined, which the recording toggle reads as a stored "disabled".
    vi.mocked(window.localStorage.getItem).mockReturnValue(null);

    // Mock crypto.randomUUID for SessionRecorder
    Object.defineProperty(global, 'crypto', {
      value: {
        randomUUID: vi.fn(() => "test-session-id-123"),
      },
      writable: true,
    });

    window.location = {
      ...window.location,
      hostname: "example.com",
      pathname: "/test",
      search: "?test=true",
      protocol: "https:",
      href: "https://example.com/test?test=true",
    };
  });

  it("should render children", () => {
    const { getByText } = render(
      <TempsAnalyticsProvider>
        <div>Test Child</div>
      </TempsAnalyticsProvider>
    );
    expect(getByText("Test Child")).toBeDefined();
  });

  it("should provide analytics context", () => {
    const { result } = renderHook(() => useTempsAnalytics(), {
      wrapper: ({ children }) => (
        <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
      ),
    });

    expect(result.current).toBeDefined();
    expect(result.current.trackEvent).toBeDefined();
    expect(result.current.trackPageview).toBeDefined();
    expect(result.current.identify).toBeDefined();
  });

  it("should respect disabled prop", async () => {
    const fetchSpy = vi.spyOn(global, "fetch");

    const { result } = renderHook(() => useTempsAnalytics(), {
      wrapper: ({ children }) => (
        <TempsAnalyticsProvider disabled>{children}</TempsAnalyticsProvider>
      ),
    });

    await result.current.trackEvent("test_event");

    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("should ignore localhost by default", async () => {
    window.location = {
      ...window.location,
      hostname: "localhost",
      protocol: "http:",
    };

    const fetchSpy = vi.spyOn(global, "fetch");

    const { result } = renderHook(() => useTempsAnalytics(), {
      wrapper: ({ children }) => (
        <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
      ),
    });

    await result.current.trackEvent("test_event");

    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("should track events on non-localhost", async () => {
    const fetchSpy = vi.spyOn(global, "fetch").mockResolvedValue(
      new Response("ok", { status: 200 })
    );

    const { result } = renderHook(() => useTempsAnalytics(), {
      wrapper: ({ children }) => (
        <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
      ),
    });

    await result.current.trackEvent("test_event", { custom: "data" });

    await waitFor(() => {
      expect(fetchSpy).toHaveBeenCalledWith(
        `${DEFAULT_BASE_PATH}/event`,
        expect.objectContaining({
          method: "POST",
          headers: {
            "Content-Type": "application/json",
          },
          body: expect.stringContaining('"event_name":"test_event"'),
        })
      );
    });
  });

  it("should use custom basePath", async () => {
    const fetchSpy = vi.spyOn(global, "fetch").mockResolvedValue(
      new Response("ok", { status: 200 })
    );

    const { result } = renderHook(() => useTempsAnalytics(), {
      wrapper: ({ children }) => (
        <TempsAnalyticsProvider basePath="/custom/path">
          {children}
        </TempsAnalyticsProvider>
      ),
    });

    await result.current.trackEvent("test_event");

    await waitFor(() => {
      expect(fetchSpy).toHaveBeenCalledWith(
        "/custom/path/event",
        expect.any(Object)
      );
    });
  });

  it("should use custom domain", async () => {
    const fetchSpy = vi.spyOn(global, "fetch").mockResolvedValue(
      new Response("ok", { status: 200 })
    );

    const { result } = renderHook(() => useTempsAnalytics(), {
      wrapper: ({ children }) => (
        <TempsAnalyticsProvider domain="custom.domain.com">
          {children}
        </TempsAnalyticsProvider>
      ),
    });

    await result.current.trackEvent("test_event");

    await waitFor(() => {
      expect(fetchSpy).toHaveBeenCalledWith(
        `${DEFAULT_BASE_PATH}/event`,
        expect.objectContaining({
          body: expect.stringContaining('"domain":"custom.domain.com"'),
        })
      );
    });
  });

  it("should track pageviews", async () => {
    const fetchSpy = vi.spyOn(global, "fetch").mockResolvedValue(
      new Response("ok", { status: 200 })
    );

    const { result } = renderHook(() => useTempsAnalytics(), {
      wrapper: ({ children }) => (
        <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
      ),
    });

    await result.current.trackPageview();

    await waitFor(() => {
      expect(fetchSpy).toHaveBeenCalledWith(
        `${DEFAULT_BASE_PATH}/event`,
        expect.objectContaining({
          method: "POST",
          body: expect.stringContaining('"event_name":"page_view"'),
        })
      );
    });
  });


  it("should auto track pageviews when enabled", () => {
    const fetchSpy = vi.spyOn(global, "fetch").mockResolvedValue(
      new Response("ok", { status: 200 })
    );

    render(
      <TempsAnalyticsProvider autoTrackPageviews>
        <div>Test</div>
      </TempsAnalyticsProvider>
    );

    expect(fetchSpy).toHaveBeenCalledWith(
      `${DEFAULT_BASE_PATH}/event`,
      expect.objectContaining({
        body: expect.stringContaining('"event_name":"page_view"'),
      })
    );
  });

  it("should not auto track pageviews when disabled", () => {
    const fetchSpy = vi.spyOn(global, "fetch");

    render(
      <TempsAnalyticsProvider autoTrackPageviews={false}>
        <div>Test</div>
      </TempsAnalyticsProvider>
    );

    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("should handle session recording when enabled", async () => {
    const fetchSpy = vi.spyOn(global, "fetch").mockResolvedValue(
      new Response("ok", { status: 201 })
    );

    render(
      <TempsAnalyticsProvider enableSessionRecording={true}>
        <div>Test</div>
      </TempsAnalyticsProvider>
    );

    // Session recorder should initialize (wait for async initialization)
    await waitFor(() => {
      const sessionInitCalls = fetchSpy.mock.calls.filter(
        call => (call[0] as string).includes("session-replay/init")
      );
      expect(sessionInitCalls.length).toBeGreaterThan(0);
    }, { timeout: 3000 });
  });

  describe("default excluded paths", () => {
    function sessionInitCalls(fetchSpy: ReturnType<typeof vi.spyOn>): unknown[] {
      return fetchSpy.mock.calls.filter((call: unknown[]) =>
        String(call[0]).includes("session-replay/init"),
      );
    }

    beforeEach(() => {
      window.location = { ...window.location, pathname: "/login", href: "https://example.com/login" };
    });

    it("does not record a built-in sensitive path through the provider", async () => {
      const fetchSpy = vi.spyOn(global, "fetch").mockResolvedValue(new Response(null, { status: 201 }));

      render(
        <TempsAnalyticsProvider enableSessionRecording={true} autoTrackPageviews={false}>
          <div>Test</div>
        </TempsAnalyticsProvider>
      );
      await new Promise((resolve) => setTimeout(resolve, 50));

      expect(sessionInitCalls(fetchSpy)).toHaveLength(0);
    });

    it("records it when useDefaultExcludedPaths is false", async () => {
      const fetchSpy = vi.spyOn(global, "fetch").mockResolvedValue(new Response(null, { status: 201 }));

      render(
        <TempsAnalyticsProvider
          enableSessionRecording={true}
          autoTrackPageviews={false}
          sessionRecordingConfig={{ useDefaultExcludedPaths: false }}
        >
          <div>Test</div>
        </TempsAnalyticsProvider>
      );

      await waitFor(() => expect(sessionInitCalls(fetchSpy)).toHaveLength(1));
    });
  });

  it("should not initialize session recording when disabled", () => {
    const fetchSpy = vi.spyOn(global, "fetch").mockResolvedValue(
      new Response("ok", { status: 200 })
    );

    render(
      <TempsAnalyticsProvider enableSessionRecording={false}>
        <div>Test</div>
      </TempsAnalyticsProvider>
    );

    // Should not call session-replay/init endpoint
    const sessionCalls = fetchSpy.mock.calls.filter(
      call => (call[0] as string).includes("session-replay")
    );
    expect(sessionCalls.length).toBe(0);
  });
});
