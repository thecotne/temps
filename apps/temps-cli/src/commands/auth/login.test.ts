// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe, beforeEach, afterEach } from 'bun:test'
import {
  debugEnabled,
  serverBaseUrl,
  absoluteVerificationUri,
  parseRetryAfterMs,
  pollForDeviceApproval,
  MAX_POLL_INTERVAL_MS,
  type DeviceApproval,
  type DevicePollDeps,
  type DeviceStartResponse,
} from './login.js'

describe('serverBaseUrl', () => {
  test('adds /api to a bare host', () => {
    expect(serverBaseUrl('https://app.temps.kfs.es')).toBe('https://app.temps.kfs.es/api')
  })

  test('leaves an already-/api-suffixed URL as-is', () => {
    expect(serverBaseUrl('https://app.temps.kfs.es/api')).toBe('https://app.temps.kfs.es/api')
  })

  test('strips a trailing slash before adding /api', () => {
    expect(serverBaseUrl('https://app.temps.kfs.es/')).toBe('https://app.temps.kfs.es/api')
  })

  test('strips multiple trailing slashes', () => {
    expect(serverBaseUrl('https://app.temps.kfs.es///')).toBe('https://app.temps.kfs.es/api')
  })
})

describe('debugEnabled', () => {
  const originalEnv = process.env.TEMPS_DEBUG

  beforeEach(() => {
    delete process.env.TEMPS_DEBUG
  })

  afterEach(() => {
    if (originalEnv === undefined) delete process.env.TEMPS_DEBUG
    else process.env.TEMPS_DEBUG = originalEnv
  })

  test('true when --debug is passed', () => {
    expect(debugEnabled({ debug: true })).toBe(true)
  })

  test('false with no flag and no env var', () => {
    expect(debugEnabled({})).toBe(false)
    expect(debugEnabled()).toBe(false)
  })

  test.each(['1', 'true', 'yes'])('TEMPS_DEBUG=%s enables debug logging', (value) => {
    process.env.TEMPS_DEBUG = value
    expect(debugEnabled({})).toBe(true)
  })

  test('an unrecognized TEMPS_DEBUG value does not enable debug logging', () => {
    process.env.TEMPS_DEBUG = 'verbose'
    expect(debugEnabled({})).toBe(false)
  })
})

describe('absoluteVerificationUri', () => {
  const start: DeviceStartResponse = {
    device_code: 'dc_123',
    user_code: 'ABCD-1234',
    verification_uri: '/cli-login',
    verification_uri_complete: '/cli-login/ABCD-1234',
    expires_in: 900,
    interval: 2,
  }

  test('returns an already-absolute URI unchanged', () => {
    const absolute = { ...start, verification_uri_complete: 'https://app.temps.kfs.es/cli-login/CODE' }
    expect(absoluteVerificationUri('https://app.temps.kfs.es', absolute)).toBe(
      'https://app.temps.kfs.es/cli-login/CODE',
    )
  })

  test('resolves a path starting with / against the base URL', () => {
    expect(absoluteVerificationUri('https://app.temps.kfs.es', start)).toBe(
      'https://app.temps.kfs.es/cli-login/ABCD-1234',
    )
  })

  test('adds the missing leading slash for a bare relative path', () => {
    const relative = { ...start, verification_uri_complete: 'cli-login/ABCD-1234' }
    expect(absoluteVerificationUri('https://app.temps.kfs.es', relative)).toBe(
      'https://app.temps.kfs.es/cli-login/ABCD-1234',
    )
  })

  test('strips a trailing slash on the base URL before joining', () => {
    expect(absoluteVerificationUri('https://app.temps.kfs.es/', start)).toBe(
      'https://app.temps.kfs.es/cli-login/ABCD-1234',
    )
  })
})

describe('parseRetryAfterMs', () => {
  const now = Date.parse('2026-01-01T00:00:00Z')

  test('reads delay-seconds', () => {
    expect(parseRetryAfterMs('60', now)).toBe(60_000)
    expect(parseRetryAfterMs(' 0 ', now)).toBe(0)
  })

  test('reads an HTTP-date relative to now', () => {
    expect(parseRetryAfterMs('Thu, 01 Jan 2026 00:00:30 GMT', now)).toBe(30_000)
  })

  test('clamps an HTTP-date in the past to zero', () => {
    expect(parseRetryAfterMs('Wed, 31 Dec 2025 23:59:00 GMT', now)).toBe(0)
  })

  test('returns null when absent or unparseable', () => {
    expect(parseRetryAfterMs(null, now)).toBeNull()
    expect(parseRetryAfterMs('', now)).toBeNull()
    expect(parseRetryAfterMs('soon', now)).toBeNull()
    expect(parseRetryAfterMs('-5', now)).toBeNull()
  })
})

describe('pollForDeviceApproval', () => {
  const pollUrl = 'https://temps.example.com/api/auth/cli/device/poll'
  const approved: DeviceApproval = {
    status: 'approved',
    user_id: 1,
    email: 'user@example.com',
    role: 'admin',
    api_key: 'tk_test',
    key_prefix: 'tk_test',
    expires_at: null,
  }

  type Reply = { status: number; body: unknown; headers?: Record<string, string> }

  const ok = (body: unknown): Reply => ({ status: 200, body })
  const pending = ok({ status: 'authorization_pending' })
  const slowDown = ok({ status: 'slow_down' })
  const tooManyRequests = (headers: Record<string, string> = {}): Reply => ({
    status: 429,
    body: 'Too many requests. Please try again later.',
    headers,
  })

  /**
   * A fake server replaying `replies` in order (repeating the last one) on
   * a fake clock, recording every sleep the poller asked for.
   */
  function harness(replies: Reply[]) {
    let clock = Date.parse('2026-01-01T00:00:00Z')
    const sleeps: number[] = []
    let polls = 0
    const deps: DevicePollDeps = {
      now: () => clock,
      sleep: async (ms) => {
        sleeps.push(ms)
        clock += ms
      },
      poll: async () => {
        const reply = replies[Math.min(polls, replies.length - 1)]!
        polls += 1
        const rawBody = typeof reply.body === 'string' ? reply.body : JSON.stringify(reply.body)
        const res = new Response(rawBody, { status: reply.status, headers: reply.headers })
        let json: unknown = null
        try {
          json = JSON.parse(rawBody)
        } catch {
          json = null
        }
        return { res, rawBody, json }
      },
    }
    return { deps, sleeps, polls: () => polls }
  }

  test('polls at the server-advertised interval until approved', async () => {
    const { deps, sleeps } = harness([pending, pending, ok(approved)])
    const result = await pollForDeviceApproval(pollUrl, { interval: 3, expires_in: 900 }, deps)
    expect(result).toEqual(approved)
    expect(sleeps).toEqual([3000, 3000, 3000])
  })

  test('slow_down adds 5 seconds to the interval for all later polls', async () => {
    const { deps, sleeps } = harness([pending, slowDown, pending, pending, ok(approved)])
    await pollForDeviceApproval(pollUrl, { interval: 2, expires_in: 900 }, deps)
    expect(sleeps).toEqual([2000, 2000, 7000, 7000, 7000])
  })

  test('a 429 backs off and honours Retry-After instead of aborting', async () => {
    const { deps, sleeps } = harness([
      pending,
      tooManyRequests({ 'Retry-After': '60' }),
      pending,
      ok(approved),
    ])
    const result = await pollForDeviceApproval(pollUrl, { interval: 2, expires_in: 900 }, deps)
    expect(result).toEqual(approved)
    // Waits the full Retry-After once, then polls at interval + 5 s.
    expect(sleeps).toEqual([2000, 2000, 60_000, 7000])
  })

  test('a 429 without Retry-After backs off like slow_down', async () => {
    const { deps, sleeps } = harness([tooManyRequests(), ok(approved)])
    await pollForDeviceApproval(pollUrl, { interval: 2, expires_in: 900 }, deps)
    expect(sleeps).toEqual([2000, 7000])
  })

  test('a 429 with an HTTP-date Retry-After waits until that date', async () => {
    // The fake clock starts at 00:00:00 and the first poll happens at 00:00:02.
    const { deps, sleeps } = harness([
      tooManyRequests({ 'Retry-After': 'Thu, 01 Jan 2026 00:00:32 GMT' }),
      ok(approved),
    ])
    await pollForDeviceApproval(pollUrl, { interval: 2, expires_in: 900 }, deps)
    expect(sleeps).toEqual([2000, 30_000])
  })

  test('repeated back-offs are capped', async () => {
    const replies: Reply[] = Array.from({ length: 20 }, () => slowDown)
    const { deps, sleeps } = harness([...replies, ok(approved)])
    await pollForDeviceApproval(pollUrl, { interval: 2, expires_in: 3600 }, deps)
    expect(Math.max(...sleeps)).toBe(MAX_POLL_INTERVAL_MS)
  })

  test('gives up at expires_in without sleeping past it', async () => {
    const { deps, sleeps, polls } = harness([pending])
    await expect(
      pollForDeviceApproval(pollUrl, { interval: 2, expires_in: 9 }, deps),
    ).rejects.toThrow('Timed out waiting for browser approval.')
    expect(sleeps).toEqual([2000, 2000, 2000, 2000, 1000])
    expect(polls()).toBe(5)
  })

  test('a server that keeps rate limiting until expiry says so', async () => {
    const { deps } = harness([tooManyRequests({ 'Retry-After': '60' })])
    await expect(
      pollForDeviceApproval(pollUrl, { interval: 2, expires_in: 120 }, deps),
    ).rejects.toThrow('the server kept rate limiting polls')
  })

  test('other error statuses still fail fast with the server detail', async () => {
    const { deps, polls } = harness([
      { status: 404, body: { title: 'Device Session Not Found', detail: 'not found' } },
    ])
    await expect(
      pollForDeviceApproval(pollUrl, { interval: 2, expires_in: 900 }, deps),
    ).rejects.toThrow('not found')
    expect(polls()).toBe(1)
  })

  test('access_denied and expired_token are terminal', async () => {
    await expect(
      pollForDeviceApproval(
        pollUrl,
        { interval: 2, expires_in: 900 },
        harness([ok({ status: 'access_denied' })]).deps,
      ),
    ).rejects.toThrow('Authorization denied in the browser.')
    await expect(
      pollForDeviceApproval(
        pollUrl,
        { interval: 2, expires_in: 900 },
        harness([ok({ status: 'expired_token' })]).deps,
      ),
    ).rejects.toThrow('Authorization code expired before approval.')
  })
})
