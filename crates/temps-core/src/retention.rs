// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Extension point for per-project ClickHouse retention resolution.
//!
//! The OTel (`spans`, `metrics`, `cross_project_trace_refs`) and proxy
//! (`proxy_logs`) ClickHouse tables store their retention window per row in a
//! `retention_days` column, and the table TTL reads that column
//! (`toDateTime(<time_col>) + toIntervalDay(retention_days)`). A
//! [`RetentionResolver`] decides the value stamped onto each new row.
//!
//! Two OSS implementations exist:
//!
//! * [`SettingsRetentionResolver`] follows the installation-wide
//!   `observability_retention` setting (`PUT /api/settings`). The OTel plugin
//!   installs it as the default for the ClickHouse `spans` and `metrics`
//!   tables, so the operator's retention choice applies there the same way it
//!   applies to the TimescaleDB retention policies.
//! * [`FixedRetentionResolver`] always returns the table default. It is what
//!   paths without a settings source use: tests, and the proxy-log writer,
//!   which runs in its own plugin context and does not read the setting yet.
//!
//! A plugin (e.g. one implementing per-project data retention policies) can
//! supply an alternative implementation — callers pass an
//! `Arc<dyn RetentionResolver>` received at construction time and the plugin
//! overrides the default by registering its implementation via the service
//! registry before the storage backends are wired up.
//!
//! Because the value is stored per row, a change only affects rows written
//! after it: rows already in ClickHouse keep the window they were written
//! with. Rewriting them would be a table-wide mutation, which Temps does not
//! start on the operator's behalf; see [`SettingsRetentionResolver`].
//!
//! `resolve` is called on the ingest path (once per row in a batch, not once
//! per HTTP request) and must be synchronous and lock-free on the read path.
//! Any expensive lookup (Postgres, external service) must be driven by a
//! background refresh task in the implementing type; the result must be cached
//! so that individual `resolve` calls do no I/O.
//!
//! [`RetentionResolverSlot`] exists because `register_services` runs in
//! plugin-registration order: the ClickHouse storage backends are constructed
//! (and their resolver captured) before a later-registered plugin gets a
//! chance to provide one (see `OtelPlugin`/`ProxyPlugin` in their respective
//! crates — the same two-phase handoff `DeploymentGate` uses via
//! `deployment_gate_slot`, adapted to a lock-free `ArcSwap` since `resolve`
//! must stay synchronous).

use std::sync::atomic::{AtomicU16, Ordering};

use crate::ObservabilityRetentionSettings;

/// Longest retention window, in days, that a `retention_days` column is
/// stamped with. Matches the upper bound the Settings API validates
/// `observability_retention` against, and fits in the column's `UInt16`.
pub const MAX_RETENTION_DAYS: u16 = 3650;

/// Which ClickHouse table a retention-days resolution is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RetentionTable {
    /// OTel spans table (`spans`) and the `cross_project_trace_refs` rows that
    /// point at them. Default TTL: 90 days.
    Spans,
    /// Proxy / request-log table (`proxy_logs`). Default TTL: 30 days.
    ProxyLogs,
    /// OTel metric points table (`metrics`). Default TTL: 90 days.
    Metrics,
}

impl RetentionTable {
    /// The table-level TTL constant in days.
    ///
    /// Matches the `DEFAULT` in the `ADD COLUMN … retention_days` migration
    /// and the prior hardcoded `INTERVAL` in the original DDL. Used by
    /// [`FixedRetentionResolver`] and as a fallback for rows that have no
    /// project context (e.g. unrouted proxy requests).
    pub fn default_days(self) -> u16 {
        match self {
            Self::Spans => 90,
            Self::ProxyLogs => 30,
            Self::Metrics => 90,
        }
    }
}

/// Convert a configured retention window into the value stored in a
/// `retention_days` column.
///
/// The Settings API rejects values outside `1..=3650`, but the settings row is
/// a JSON document that predates that validation and can be edited directly.
/// Clamping here keeps a bad stored value from ever reaching ClickHouse: `0`
/// would make every new row expire on arrival, and anything above
/// [`MAX_RETENTION_DAYS`] would overflow the `UInt16` column.
pub fn clamp_retention_days(days: u32) -> u16 {
    // The clamp bounds the value to 1..=3650, so the cast cannot truncate.
    days.clamp(1, u32::from(MAX_RETENTION_DAYS)) as u16
}

/// Extension point for resolving the effective `retention_days` to stamp onto
/// an ingested ClickHouse row.
///
/// OSS resolves every project to the installation-wide setting
/// ([`SettingsRetentionResolver`]) or, where no settings source exists, to
/// [`RetentionTable::default_days`] ([`FixedRetentionResolver`]). A plugin
/// (e.g. one implementing per-project data retention policies) registers an
/// implementation via the service registry — `context.register_service(resolver)`
/// — only when appropriate (e.g. gated by its own licensing check). Storage
/// backends receive the resolver at construction time as
/// `Arc<dyn RetentionResolver>`, so a plugin-free binary needs no extra
/// configuration and the ClickHouse rows self-expire at the per-row TTL.
///
/// Implementations should handle every [`RetentionTable`] variant; a table
/// added later (as [`RetentionTable::Metrics`] was) is a compile error for an
/// exhaustive `match`, which is the point: the implementor decides what that
/// table's per-project window is instead of inheriting one silently.
pub trait RetentionResolver: Send + Sync {
    /// Return the effective `retention_days` for `project_id` in `table`.
    ///
    /// The returned value is written into the `retention_days` column of each
    /// new row, where it drives the per-row TTL expression
    /// `toDateTime(<time_col>) + toIntervalDay(retention_days)`.
    ///
    /// Implementations must be synchronous and must not perform I/O — see the
    /// module-level note.
    fn resolve(&self, project_id: i32, table: RetentionTable) -> u16;
}

/// [`RetentionResolver`] that returns the ClickHouse table-level default for
/// every project.
///
/// Used where no settings source is available (tests, the proxy-log writer).
/// Callers with no project context (e.g. unrouted proxy requests where
/// `project_id` is `NULL`) should use [`RetentionTable::default_days`]
/// directly rather than passing a fabricated project ID to this resolver.
pub struct FixedRetentionResolver;

impl RetentionResolver for FixedRetentionResolver {
    fn resolve(&self, _project_id: i32, table: RetentionTable) -> u16 {
        table.default_days()
    }
}

/// [`RetentionResolver`] that follows the installation-wide
/// `observability_retention` setting.
///
/// Every project resolves to the same value per table: `otel_spans_days` for
/// [`RetentionTable::Spans`], `otel_metrics_days` for
/// [`RetentionTable::Metrics`] and `proxy_logs_days` for
/// [`RetentionTable::ProxyLogs`]. Until [`Self::apply`] is first called it
/// returns the table defaults, i.e. exactly what [`FixedRetentionResolver`]
/// returns, so a settings read that fails at startup degrades to the
/// behaviour that existed before this type.
///
/// The owner keeps it current by calling [`Self::apply`] with the latest
/// settings (the OTel plugin re-reads them once a minute). Each value is an
/// independent atomic, so `resolve` stays lock-free and allocation-free on the
/// ingest path; a batch that races an update may stamp some rows with the old
/// value and some with the new one, which is harmless because every row
/// carries its own window.
///
/// # Rows that are already stored
///
/// A new value applies to rows ingested after it takes effect. ClickHouse
/// evaluates the TTL from the `retention_days` stored on each row, so rows
/// written earlier keep the window they were written with (the previous
/// setting, or the 90-day default), and a shortened window therefore takes
/// full effect on disk once the old window has elapsed. Rewriting the stored
/// rows (`ALTER TABLE spans UPDATE retention_days = N WHERE ...`) is a
/// mutation over every part of the table. That is too heavy to start on the
/// operator's behalf on a small host, and it would also override any
/// per-project window a retention plugin stamped, so it is left to the
/// operator to run deliberately.
#[derive(Debug)]
pub struct SettingsRetentionResolver {
    spans_days: AtomicU16,
    metrics_days: AtomicU16,
    proxy_logs_days: AtomicU16,
}

impl SettingsRetentionResolver {
    /// Start at the table defaults; call [`Self::apply`] to load settings.
    pub fn new() -> Self {
        Self {
            spans_days: AtomicU16::new(RetentionTable::Spans.default_days()),
            metrics_days: AtomicU16::new(RetentionTable::Metrics.default_days()),
            proxy_logs_days: AtomicU16::new(RetentionTable::ProxyLogs.default_days()),
        }
    }

    /// Build a resolver already holding `settings`.
    pub fn from_settings(settings: &ObservabilityRetentionSettings) -> Self {
        let resolver = Self::new();
        resolver.apply(settings);
        resolver
    }

    /// Load the retention windows from `settings`, clamped with
    /// [`clamp_retention_days`].
    ///
    /// Returns `true` when at least one table's value changed, so the caller
    /// can log a change once instead of on every refresh.
    pub fn apply(&self, settings: &ObservabilityRetentionSettings) -> bool {
        let updates = [
            (&self.spans_days, settings.otel_spans_days),
            (&self.metrics_days, settings.otel_metrics_days),
            (&self.proxy_logs_days, settings.proxy_logs_days),
        ];
        let mut changed = false;
        for (slot, days) in updates {
            let days = clamp_retention_days(days);
            // Relaxed is enough: each value is independent and readers only
            // need to observe some recent value, not a consistent snapshot.
            if slot.swap(days, Ordering::Relaxed) != days {
                changed = true;
            }
        }
        changed
    }

    /// The `retention_days` currently stamped onto new rows of `table`.
    pub fn days(&self, table: RetentionTable) -> u16 {
        let slot = match table {
            RetentionTable::Spans => &self.spans_days,
            RetentionTable::Metrics => &self.metrics_days,
            RetentionTable::ProxyLogs => &self.proxy_logs_days,
        };
        slot.load(Ordering::Relaxed)
    }
}

impl Default for SettingsRetentionResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl RetentionResolver for SettingsRetentionResolver {
    fn resolve(&self, _project_id: i32, table: RetentionTable) -> u16 {
        self.days(table)
    }
}

/// Deferred-registration handle for a [`RetentionResolver`].
///
/// Constructed with a default resolver loaded ([`FixedRetentionResolver`] via
/// [`Self::new_default`], or any other via [`Self::with_default`]) and handed
/// to a storage backend immediately at construction time (as
/// `Arc<dyn RetentionResolver>`, via unsized coercion — this type itself
/// implements the trait). Once every plugin has finished `register_services`,
/// whichever plugin owns the slot calls [`Self::set`] from
/// `initialize_plugin_services` if a plugin registered an alternative
/// resolver — see the module-level note for why this indirection exists.
/// `resolve` reads are lock-free (`ArcSwap::load`).
///
/// **Write-once semantics:** only the first call to [`Self::set`] takes
/// effect. A second caller cannot silently overwrite a resolver that was
/// already installed — the call is a no-op and returns `false`. This
/// prevents a buggy or late-registered plugin from replacing a correctly
/// wired resolver after the fact.
pub struct RetentionResolverSlot {
    resolver: arc_swap::ArcSwap<std::sync::Arc<dyn RetentionResolver>>,
    /// Flipped to `true` by the first successful [`Self::set`] call.
    claimed: std::sync::atomic::AtomicBool,
}

impl RetentionResolverSlot {
    /// Start with [`FixedRetentionResolver`] loaded.
    pub fn new_default() -> Self {
        Self::with_default(std::sync::Arc::new(FixedRetentionResolver))
    }

    /// Start with `resolver` loaded until a plugin claims the slot.
    ///
    /// Loading the default does not claim the slot: [`Self::set`] still
    /// accepts exactly one plugin-provided resolver afterwards.
    pub fn with_default(resolver: std::sync::Arc<dyn RetentionResolver>) -> Self {
        Self {
            resolver: arc_swap::ArcSwap::new(std::sync::Arc::new(resolver)),
            claimed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Swap in a resolver provided by a plugin, but only once.
    ///
    /// A resolver that has already been set (by a prior `set()` call) cannot
    /// be silently overwritten by a second plugin. Returns `true` if the swap
    /// was applied, `false` if a resolver was already set and this call was a
    /// no-op.
    pub fn set(&self, resolver: std::sync::Arc<dyn RetentionResolver>) -> bool {
        if self
            .claimed
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
        {
            self.resolver.store(std::sync::Arc::new(resolver));
            true
        } else {
            false
        }
    }
}

impl RetentionResolver for RetentionResolverSlot {
    fn resolve(&self, project_id: i32, table: RetentionTable) -> u16 {
        self.resolver.load().resolve(project_id, table)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_TABLES: [RetentionTable; 3] = [
        RetentionTable::Spans,
        RetentionTable::ProxyLogs,
        RetentionTable::Metrics,
    ];

    fn retention_settings(
        spans: u32,
        metrics: u32,
        proxy_logs: u32,
    ) -> ObservabilityRetentionSettings {
        ObservabilityRetentionSettings {
            otel_spans_days: spans,
            otel_metrics_days: metrics,
            proxy_logs_days: proxy_logs,
            ..ObservabilityRetentionSettings::default()
        }
    }

    #[test]
    fn fixed_resolver_returns_table_defaults() {
        let r = FixedRetentionResolver;
        assert_eq!(r.resolve(1, RetentionTable::Spans), 90);
        assert_eq!(r.resolve(1, RetentionTable::ProxyLogs), 30);
        assert_eq!(r.resolve(1, RetentionTable::Metrics), 90);
        // project_id is ignored
        assert_eq!(r.resolve(99999, RetentionTable::Spans), 90);
        assert_eq!(r.resolve(0, RetentionTable::ProxyLogs), 30);
    }

    #[test]
    fn retention_table_default_days() {
        assert_eq!(RetentionTable::Spans.default_days(), 90);
        assert_eq!(RetentionTable::ProxyLogs.default_days(), 30);
        assert_eq!(RetentionTable::Metrics.default_days(), 90);
    }

    /// The column defaults in the ClickHouse migrations and the settings
    /// defaults must agree, or a fresh install would stamp a different window
    /// than an upgraded one reports.
    #[test]
    fn table_defaults_match_settings_defaults() {
        let defaults = ObservabilityRetentionSettings::default();
        assert_eq!(
            u32::from(RetentionTable::Spans.default_days()),
            defaults.otel_spans_days
        );
        assert_eq!(
            u32::from(RetentionTable::Metrics.default_days()),
            defaults.otel_metrics_days
        );
        assert_eq!(
            u32::from(RetentionTable::ProxyLogs.default_days()),
            defaults.proxy_logs_days
        );
    }

    #[test]
    fn clamp_retention_days_keeps_the_supported_range() {
        assert_eq!(clamp_retention_days(1), 1);
        assert_eq!(clamp_retention_days(7), 7);
        assert_eq!(clamp_retention_days(3650), 3650);
    }

    #[test]
    fn clamp_retention_days_never_stamps_zero() {
        // A zero-day window would expire every new row on arrival.
        assert_eq!(clamp_retention_days(0), 1);
    }

    #[test]
    fn clamp_retention_days_caps_values_the_column_cannot_hold() {
        assert_eq!(clamp_retention_days(3651), MAX_RETENTION_DAYS);
        // Above u16::MAX: a plain `as u16` would wrap to a short window.
        assert_eq!(clamp_retention_days(65_543), MAX_RETENTION_DAYS);
        assert_eq!(clamp_retention_days(u32::MAX), MAX_RETENTION_DAYS);
    }

    #[test]
    fn settings_resolver_starts_at_the_fixed_defaults() {
        let settings = SettingsRetentionResolver::new();
        for table in ALL_TABLES {
            assert_eq!(
                settings.resolve(1, table),
                FixedRetentionResolver.resolve(1, table),
                "{table:?} must start at the table default"
            );
        }
        assert_eq!(
            SettingsRetentionResolver::default().days(RetentionTable::Spans),
            90
        );
    }

    #[test]
    fn settings_resolver_maps_each_setting_to_its_table() {
        let resolver = SettingsRetentionResolver::new();
        assert!(resolver.apply(&retention_settings(7, 14, 3)));

        assert_eq!(resolver.days(RetentionTable::Spans), 7);
        assert_eq!(resolver.days(RetentionTable::Metrics), 14);
        assert_eq!(resolver.days(RetentionTable::ProxyLogs), 3);
        // Installation-wide: every project resolves to the same value.
        assert_eq!(resolver.resolve(1, RetentionTable::Spans), 7);
        assert_eq!(resolver.resolve(424_242, RetentionTable::Metrics), 14);
    }

    #[test]
    fn settings_resolver_ignores_settings_it_does_not_stamp() {
        // otel_logs_days and container_logs_days have no ClickHouse
        // retention_days column; changing them must not report a change.
        let resolver = SettingsRetentionResolver::new();
        let settings = ObservabilityRetentionSettings {
            otel_logs_days: 5,
            container_logs_days: 5,
            ..ObservabilityRetentionSettings::default()
        };
        assert!(!resolver.apply(&settings));
        for table in ALL_TABLES {
            assert_eq!(resolver.days(table), table.default_days());
        }
    }

    #[test]
    fn settings_resolver_apply_reports_changes_only_once() {
        let resolver = SettingsRetentionResolver::new();
        let settings = retention_settings(7, 90, 30);
        assert!(resolver.apply(&settings), "spans changed 90 -> 7");
        assert!(!resolver.apply(&settings), "re-applying is not a change");

        assert!(resolver.apply(&retention_settings(7, 30, 30)));
        assert_eq!(resolver.days(RetentionTable::Metrics), 30);
    }

    #[test]
    fn settings_resolver_can_widen_the_window_again() {
        let resolver = SettingsRetentionResolver::from_settings(&retention_settings(7, 7, 7));
        assert_eq!(resolver.days(RetentionTable::Spans), 7);
        assert!(resolver.apply(&retention_settings(365, 365, 365)));
        for table in ALL_TABLES {
            assert_eq!(resolver.days(table), 365);
        }
    }

    #[test]
    fn settings_resolver_clamps_out_of_range_settings() {
        let resolver = SettingsRetentionResolver::from_settings(&retention_settings(0, 100_000, 1));
        assert_eq!(resolver.days(RetentionTable::Spans), 1);
        assert_eq!(resolver.days(RetentionTable::Metrics), MAX_RETENTION_DAYS);
        assert_eq!(resolver.days(RetentionTable::ProxyLogs), 1);
    }

    #[test]
    fn slot_with_default_resolves_through_the_given_resolver() {
        let settings = std::sync::Arc::new(SettingsRetentionResolver::from_settings(
            &retention_settings(7, 14, 30),
        ));
        let slot = RetentionResolverSlot::with_default(settings.clone());
        assert_eq!(slot.resolve(1, RetentionTable::Spans), 7);
        assert_eq!(slot.resolve(1, RetentionTable::Metrics), 14);

        // The slot holds the same instance, so a later settings refresh is
        // visible to storage that captured the slot at construction time.
        settings.apply(&retention_settings(3, 5, 30));
        assert_eq!(slot.resolve(1, RetentionTable::Spans), 3);
        assert_eq!(slot.resolve(1, RetentionTable::Metrics), 5);
    }

    #[test]
    fn slot_with_default_is_not_claimed() {
        // Loading a default must leave room for exactly one plugin resolver.
        let settings = std::sync::Arc::new(SettingsRetentionResolver::from_settings(
            &retention_settings(7, 7, 7),
        ));
        let slot = RetentionResolverSlot::with_default(settings);
        assert!(slot.set(std::sync::Arc::new(AlwaysForty)));
        assert_eq!(slot.resolve(1, RetentionTable::Spans), 40);
        assert!(!slot.set(std::sync::Arc::new(AlwaysSeven)));
        assert_eq!(slot.resolve(1, RetentionTable::Metrics), 40);
    }

    struct AlwaysSeven;
    impl RetentionResolver for AlwaysSeven {
        fn resolve(&self, _project_id: i32, _table: RetentionTable) -> u16 {
            7
        }
    }

    #[test]
    fn slot_defaults_to_fixed_resolver() {
        let slot = RetentionResolverSlot::new_default();
        assert_eq!(slot.resolve(1, RetentionTable::Spans), 90);
        assert_eq!(slot.resolve(1, RetentionTable::ProxyLogs), 30);
        assert_eq!(slot.resolve(1, RetentionTable::Metrics), 90);
    }

    #[test]
    fn slot_set_overrides_the_default() {
        let slot = RetentionResolverSlot::new_default();
        let swapped = slot.set(std::sync::Arc::new(AlwaysSeven));
        assert!(swapped, "first set() must return true");
        assert_eq!(slot.resolve(1, RetentionTable::Spans), 7);
        assert_eq!(slot.resolve(1, RetentionTable::ProxyLogs), 7);
    }

    struct AlwaysForty;
    impl RetentionResolver for AlwaysForty {
        fn resolve(&self, _project_id: i32, _table: RetentionTable) -> u16 {
            40
        }
    }

    #[test]
    fn slot_second_set_is_noop() {
        let slot = RetentionResolverSlot::new_default();

        // First set: succeeds and resolver is active.
        let first = slot.set(std::sync::Arc::new(AlwaysSeven));
        assert!(first, "first set() must return true");
        assert_eq!(slot.resolve(1, RetentionTable::Spans), 7);

        // Second set: must be a no-op — returns false, resolver unchanged.
        let second = slot.set(std::sync::Arc::new(AlwaysForty));
        assert!(!second, "second set() must return false (write-once)");

        // The FIRST resolver is still active; AlwaysForty must not have taken effect.
        assert_eq!(
            slot.resolve(1, RetentionTable::Spans),
            7,
            "resolver must remain AlwaysSeven after rejected second set()"
        );
        assert_eq!(
            slot.resolve(1, RetentionTable::ProxyLogs),
            7,
            "resolver must remain AlwaysSeven after rejected second set()"
        );
    }
}
