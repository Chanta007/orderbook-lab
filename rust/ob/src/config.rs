//! The flat config reader, and the two safety gates that live in it.
//!
//! # Why not a JSON crate
//!
//! The config files are flat objects of strings, numbers and booleans. A general
//! JSON parser would be a dependency, a grammar, and a set of error messages
//! about nesting that this lab never produces. This reader finds a key, reads
//! the value after the colon, and stops at a comma or a brace — the same thing
//! the C++ loader does, deliberately, so that both implementations accept
//! exactly the same files and reject exactly the same malformed ones.
//!
//! # The safety gates
//!
//! Two checks, both fatal, and they are the whole safety policy:
//!
//! 1. A config with `"env": "prod"` will not load unless `ORDERBOOK_ALLOW_PROD=1`
//!    is set in the environment. Starting the production profile has to be a
//!    deliberate act, not the result of a typo in a path.
//! 2. A config with `"env": "dev"` will not load if `allow_orders` is true. The
//!    lab cannot place an order at all — the adapter talks to a market-data-only
//!    host — and this gate means a config cannot even claim otherwise.
//!
//! `python/obctl.py` repeats both checks before it starts any process, so a
//! refused config is refused twice, by two implementations, in two languages.

use crate::error::{Error, Result};
use crate::msg::SYMBOL_LEN;
use std::path::Path;

/// Environment variable that unlocks the production profile.
pub const ALLOW_PROD_ENV: &str = "ORDERBOOK_ALLOW_PROD";

/// A loaded, validated configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// `dev` or `prod`.
    pub env: String,
    /// Where the shared-memory ring lives.
    pub bus_path: String,
    /// Where the write-ahead log lives.
    pub wal_path: String,
    /// Where the structured log lines go.
    pub log_path: String,
    /// Address the feed daemon binds.
    pub listen_host: String,
    /// Port the feed daemon binds.
    pub listen_port: u16,
    /// The symbol used when a record arrives with an empty symbol field.
    pub symbol: String,
    /// Whether this config claims orders are possible. Must be false in dev.
    pub allow_orders: bool,
    /// Levels the terminal shows by default.
    pub levels: usize,

    // --- the ticker picker ---------------------------------------------------
    /// The file the terminal writes to say which symbols to subscribe to.
    pub watch_path: String,
    /// The list the picker suggests from.
    pub tickers_path: String,

    // --- settings the C++ loader does not have -------------------------------
    /// Slots in the shared-memory ring. Must be a power of two.
    pub ring_capacity: u32,
    /// How much of the log's tail a crash may lose.
    pub sync_policy: crate::wal::SyncPolicy,
    /// Symbol-table capacity for the ticker cache.
    pub cache_symbols: usize,
    /// Cache shards.
    pub cache_shards: usize,
    /// Levels kept per side per symbol in the cache.
    pub cache_depth: usize,
    /// Subscriber queues the cache creates.
    pub cache_subscribers: usize,
    /// Records each subscriber queue holds.
    pub cache_queue: usize,
    /// What a subscriber queue does when it is full.
    pub cache_policy: crate::flow::DropPolicy,
    /// Lag at which a subscriber is reported as elevated.
    pub pressure_elevated_ns: u64,
    /// Lag at which a subscriber is reported as saturated.
    pub pressure_saturated_ns: u64,
    /// Expected record interval, for the coordinated-omission correction.
    pub expected_interval_ns: u64,
}

impl Default for Config {
    /// The dev defaults, which are also what a config file that omits a key
    /// gets. Every one of these matches the C++ loader's default where the two
    /// share a key.
    fn default() -> Self {
        Config {
            env: "dev".to_string(),
            bus_path: "var/dev/bus".to_string(),
            wal_path: "var/dev/wal.bin".to_string(),
            log_path: "var/dev/async.log".to_string(),
            listen_host: "127.0.0.1".to_string(),
            listen_port: 9001,
            symbol: "BTCUSDT".to_string(),
            allow_orders: false,
            levels: 5,
            watch_path: "var/dev/watch.txt".to_string(),
            tickers_path: "config/tickers.txt".to_string(),
            ring_capacity: crate::ring::DEFAULT_CAPACITY,
            sync_policy: crate::wal::SyncPolicy::Data,
            cache_symbols: 4096,
            cache_shards: 4,
            cache_depth: 16,
            cache_subscribers: 2,
            cache_queue: 8192,
            cache_policy: crate::flow::DropPolicy::DropOldest,
            pressure_elevated_ns: 1_000_000,
            pressure_saturated_ns: 10_000_000,
            expected_interval_ns: 100_000,
        }
    }
}

impl Config {
    /// Read and validate a config file.
    pub fn load(path: &Path) -> Result<Self> {
        let body = std::fs::read_to_string(path).map_err(|e| Error::io("read config", e))?;
        Self::from_str(&body)
    }

    /// Parse and validate a config body.
    ///
    /// Named for what it does rather than to satisfy the trait convention: this
    /// is not `FromStr` because it applies the safety gates, so it is fallible
    /// for reasons a `FromStr` impl is not expected to have.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(body: &str) -> Result<Self> {
        let d = Config::default();
        // The environment is read first, because two defaults depend on it.
        let env = json_str(body, "env", &d.env);
        let bus_path = json_str(body, "bus_path", &d.bus_path);
        // The watch file sits **beside the bus**, derived from `bus_path` rather
        // than from `env`, and the Python adapter derives it the same way. One
        // rule, because two rules for one key is two files: the first version of
        // this defaulted to `var/<env>/watch.txt` while the adapter looked beside
        // the bus, so the terminal wrote one file and the adapter read another and
        // nothing was ever subscribed to. A config with an absolute `bus_path`
        // makes the difference visible, which is how it was found.
        let watch_default = watch_path_beside(&bus_path);
        let c = Config {
            env,
            bus_path,
            wal_path: json_str(body, "wal_path", &d.wal_path),
            log_path: json_str(body, "log_path", &d.log_path),
            listen_host: json_str(body, "listen_host", &d.listen_host),
            listen_port: json_u64(body, "listen_port", d.listen_port as u64) as u16,
            symbol: json_str(body, "symbol", &d.symbol),
            allow_orders: json_bool(body, "allow_orders", d.allow_orders),
            levels: json_u64(body, "levels", d.levels as u64) as usize,
            watch_path: json_str(body, "watch_path", &watch_default),
            tickers_path: json_str(body, "tickers_path", &d.tickers_path),
            ring_capacity: json_u64(body, "ring_capacity", d.ring_capacity as u64) as u32,
            sync_policy: match json_str(body, "sync_policy", "data").as_str() {
                "none" => crate::wal::SyncPolicy::None,
                "full" => crate::wal::SyncPolicy::Full,
                _ => crate::wal::SyncPolicy::Data,
            },
            cache_symbols: json_u64(body, "cache_symbols", d.cache_symbols as u64) as usize,
            cache_shards: json_u64(body, "cache_shards", d.cache_shards as u64) as usize,
            cache_depth: json_u64(body, "cache_depth", d.cache_depth as u64) as usize,
            cache_subscribers: json_u64(body, "cache_subscribers", d.cache_subscribers as u64)
                as usize,
            cache_queue: json_u64(body, "cache_queue", d.cache_queue as u64) as usize,
            cache_policy: match json_str(body, "cache_policy", "drop-oldest").as_str() {
                "drop-newest" => crate::flow::DropPolicy::DropNewest,
                "credit" => crate::flow::DropPolicy::Credit,
                "spin" => crate::flow::DropPolicy::Spin { spins: 64 },
                _ => crate::flow::DropPolicy::DropOldest,
            },
            pressure_elevated_ns: json_u64(body, "pressure_elevated_ns", d.pressure_elevated_ns),
            pressure_saturated_ns: json_u64(body, "pressure_saturated_ns", d.pressure_saturated_ns),
            expected_interval_ns: json_u64(body, "expected_interval_ns", d.expected_interval_ns),
        };
        c.validate()?;
        Ok(c)
    }

    /// Apply the two safety gates and the structural checks.
    ///
    /// # Errors
    ///
    /// - `env: prod` without `ORDERBOOK_ALLOW_PROD=1`
    /// - `env: dev` with `allow_orders: true`
    /// - an empty symbol, or one too long for the wire field
    /// - a ring capacity that is not a power of two
    pub fn validate(&self) -> Result<()> {
        if self.env == "prod" {
            let allow = std::env::var(ALLOW_PROD_ENV).unwrap_or_default();
            if allow != "1" {
                return Err(Error::Config(format!(
                    "prod config refused (set {ALLOW_PROD_ENV}=1)"
                )));
            }
        }
        if self.env == "dev" && self.allow_orders {
            return Err(Error::Config(
                "dev config must not set allow_orders".to_string(),
            ));
        }
        if self.symbol.is_empty() {
            return Err(Error::Config("symbol must not be empty".to_string()));
        }
        if self.symbol.len() > SYMBOL_LEN - 1 {
            return Err(Error::Config(format!(
                "symbol {} is longer than the {} bytes the wire record holds",
                self.symbol,
                SYMBOL_LEN - 1
            )));
        }
        if !self.ring_capacity.is_power_of_two() {
            return Err(Error::Config(format!(
                "ring_capacity {} must be a power of two",
                self.ring_capacity
            )));
        }
        if self.pressure_elevated_ns >= self.pressure_saturated_ns {
            return Err(Error::Config(format!(
                "pressure_elevated_ns {} must be below pressure_saturated_ns {}",
                self.pressure_elevated_ns, self.pressure_saturated_ns
            )));
        }
        Ok(())
    }

    /// The cache configuration these settings describe.
    pub fn cache_config(&self) -> crate::cache::CacheConfig {
        crate::cache::CacheConfig {
            symbols: self.cache_symbols,
            shards: self.cache_shards,
            depth: self.cache_depth,
            subscribers: self.cache_subscribers,
            queue: self.cache_queue,
            policy: self.cache_policy,
            latency: true,
            slippage: true,
            pressure: crate::pressure::PressureConfig {
                elevated_ns: self.pressure_elevated_ns,
                saturated_ns: self.pressure_saturated_ns,
                enabled: true,
            },
            expected_interval_ns: self.expected_interval_ns,
            horizons: crate::slippage::DEFAULT_HORIZONS_NS,
        }
    }
}

/// Find `"key"`, then the value after the colon, and read it as a string.
///
/// A string value is read to its closing quote. Anything else is read to the
/// next comma, brace or newline and trimmed. Nested objects are not supported
/// and would produce a wrong answer rather than an error, which is why the
/// config files are flat by policy.
pub fn json_str(body: &str, key: &str, default: &str) -> String {
    let needle = format!("\"{key}\"");
    let Some(mut p) = body.find(&needle) else {
        return default.to_string();
    };
    p = match body[p..].find(':') {
        Some(off) => p + off,
        None => return default.to_string(),
    };
    let bytes = body.as_bytes();
    let mut q = p + 1;
    while q < bytes.len() && (bytes[q] as char).is_whitespace() {
        q += 1;
    }
    if q >= bytes.len() {
        return default.to_string();
    }
    if bytes[q] == b'"' {
        let start = q + 1;
        let Some(end) = body[start..].find('"') else {
            return default.to_string();
        };
        return body[start..start + end].to_string();
    }
    let end = body[q..]
        .find([',', '}', '\n'])
        .map(|off| q + off)
        .unwrap_or(bytes.len());
    body[q..end].trim().to_string()
}

/// The watch file that belongs beside a bus.
///
/// One rule for both implementations: the directory holding the bus holds the
/// watch list. Derived rather than configured so there is no second path to keep
/// in step, and a config that moves the bus moves the watch list with it.
pub fn watch_path_beside(bus_path: &str) -> String {
    match std::path::Path::new(bus_path).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            parent.join("watch.txt").to_string_lossy().into_owned()
        }
        _ => "watch.txt".to_string(),
    }
}

/// Read a key as an unsigned integer, falling back to `default`.
///
/// A value that is present but not a number also falls back, and the caller is
/// expected to notice via [`Config::validate`] when the value matters.
pub fn json_u64(body: &str, key: &str, default: u64) -> u64 {
    let raw = json_str(body, key, "");
    if raw.is_empty() {
        return default;
    }
    raw.parse().unwrap_or(default)
}

/// Read a key as a boolean.
pub fn json_bool(body: &str, key: &str, default: bool) -> bool {
    let raw = json_str(body, key, "");
    match raw.as_str() {
        "true" => true,
        "false" => false,
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEV: &str = r#"{
  "env": "dev",
  "bus_path": "var/dev/bus",
  "wal_path": "var/dev/wal.bin",
  "log_path": "var/dev/async.log",
  "listen_host": "127.0.0.1",
  "listen_port": 9001,
  "symbol": "BTCUSDT",
  "stream": "btcusdt@depth5@100ms",
  "ws_url": "wss://data-stream.binance.vision:443/ws/btcusdt@depth5@100ms",
  "allow_orders": false,
  "levels": 5
}"#;

    const PROD: &str = r#"{
  "env": "prod",
  "bus_path": "var/prod/bus",
  "wal_path": "var/prod/wal.bin",
  "log_path": "var/prod/async.log",
  "listen_host": "127.0.0.1",
  "listen_port": 9101,
  "symbol": "BTCUSDT",
  "allow_orders": false,
  "levels": 5
}"#;

    #[test]
    fn the_shipped_dev_config_parses_and_matches_the_cpp_defaults() {
        let c = Config::from_str(DEV).expect("dev config");
        assert_eq!(c.env, "dev");
        assert_eq!(c.bus_path, "var/dev/bus");
        assert_eq!(c.wal_path, "var/dev/wal.bin");
        assert_eq!(c.listen_host, "127.0.0.1");
        assert_eq!(c.listen_port, 9001);
        assert_eq!(c.symbol, "BTCUSDT");
        assert!(!c.allow_orders);
        assert_eq!(c.levels, 5);
        // Keys the C++ loader ignores are read here, and unknown keys such as
        // `stream` and `ws_url` are left alone rather than rejected.
        assert_eq!(c.ring_capacity, crate::ring::DEFAULT_CAPACITY);
    }

    /// Serialises the tests that read or write `ALLOW_PROD_ENV`.
    ///
    /// `set_var` changes the whole process, and Rust runs tests in parallel threads:
    /// a test that sets the variable while another is deciding whether a prod config
    /// should load makes an assertion that contradicts itself. It did, in CI — the
    /// variable was read by the assertion and read again by the loader underneath it,
    /// and a flip in between made `true == false` in a test that had passed a minute
    /// earlier. The lock is what makes "the environment" something these three tests
    /// can reason about.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The lock, ignoring poisoning: a test that panicked while holding it has
    /// already reported, and the next one should still run.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn a_prod_config_is_refused_without_the_environment_variable() {
        let _env = env_lock();
        // The test process does not set it; if the surrounding environment does,
        // the assertion below is the other branch and both are correct.
        let allowed = std::env::var(ALLOW_PROD_ENV).as_deref() == Ok("1");
        let result = Config::from_str(PROD);
        if allowed {
            assert!(result.is_ok());
            assert_eq!(result.unwrap().listen_port, 9101);
        } else {
            let err = result.expect_err("prod must be refused");
            assert!(err.to_string().contains("prod config refused"), "{err}");
        }
    }

    #[test]
    fn a_dev_config_that_claims_orders_are_possible_is_refused() {
        let body = DEV.replace("\"allow_orders\": false", "\"allow_orders\": true");
        let err = Config::from_str(&body).expect_err("must be refused");
        assert!(err.to_string().contains("allow_orders"), "{err}");
    }

    #[test]
    fn a_missing_key_falls_back_to_the_default() {
        let c = Config::from_str("{}").expect("empty config");
        assert_eq!(c, Config::default());
    }

    #[test]
    fn values_are_read_up_to_the_delimiter() {
        assert_eq!(json_str(r#"{"a": 12, "b": 3}"#, "a", "x"), "12");
        assert_eq!(
            json_str(r#"{"a": "hi there", "b": 3}"#, "a", "x"),
            "hi there"
        );
        assert_eq!(json_str(r#"{"a": true}"#, "a", "x"), "true");
        assert_eq!(json_str(r#"{"a": 1}"#, "b", "x"), "x");
        assert_eq!(json_u64(r#"{"n": 4096}"#, "n", 1), 4096);
        assert_eq!(json_u64(r#"{"n": "nope"}"#, "n", 1), 1);
        assert_eq!(json_u64(r#"{}"#, "n", 1), 1);
        assert!(json_bool(r#"{"b": true}"#, "b", false));
        assert!(!json_bool(r#"{"b": false}"#, "b", true));
        assert!(json_bool(r#"{}"#, "b", true));
    }

    #[test]
    fn a_key_that_is_a_prefix_of_another_is_not_confused() {
        // `env` appears inside `env_name`; the quoted needle prevents a match.
        let body = r#"{"env_name": "other", "env": "dev"}"#;
        assert_eq!(json_str(body, "env", "x"), "dev");
    }

    #[test]
    fn the_new_cache_keys_are_read() {
        let body = r#"{
            "env": "dev",
            "ring_capacity": 8192,
            "sync_policy": "full",
            "cache_symbols": 2048,
            "cache_shards": 8,
            "cache_depth": 32,
            "cache_subscribers": 3,
            "cache_queue": 1024,
            "cache_policy": "drop-newest",
            "pressure_elevated_ns": 2000000,
            "pressure_saturated_ns": 20000000,
            "expected_interval_ns": 50000
        }"#;
        let c = Config::from_str(body).expect("config");
        assert_eq!(c.ring_capacity, 8192);
        assert_eq!(c.sync_policy, crate::wal::SyncPolicy::Full);
        assert_eq!(c.cache_symbols, 2048);
        assert_eq!(c.cache_shards, 8);
        assert_eq!(c.cache_depth, 32);
        assert_eq!(c.cache_subscribers, 3);
        assert_eq!(c.cache_queue, 1024);
        assert_eq!(c.cache_policy, crate::flow::DropPolicy::DropNewest);
        assert_eq!(c.pressure_elevated_ns, 2_000_000);
        assert_eq!(c.pressure_saturated_ns, 20_000_000);
        assert_eq!(c.expected_interval_ns, 50_000);

        let cache = c.cache_config();
        assert_eq!(cache.symbols, 2048);
        assert_eq!(cache.queue, 1024);
        assert_eq!(cache.policy, crate::flow::DropPolicy::DropNewest);
    }

    #[test]
    fn the_watch_file_defaults_to_sitting_beside_the_bus() {
        // Derived from `bus_path`, which is the rule the Python adapter uses too.
        // Two rules for one key means the terminal writes one file and the adapter
        // reads another, and nothing is ever subscribed to.
        assert_eq!(
            Config::from_str(DEV).expect("dev").watch_path,
            "var/dev/watch.txt"
        );
        // The shipped prod config's bus is in var/prod, so its watch file is too.
        let prod = r#"{"env": "prod", "bus_path": "var/prod/bus"}"#;
        // Held across the set, the parse and the removal, so no other test can see
        // the variable set or unset halfway through.
        let _env = env_lock();
        std::env::set_var(ALLOW_PROD_ENV, "1");
        let parsed = Config::from_str(prod).expect("prod");
        std::env::remove_var(ALLOW_PROD_ENV);
        assert_eq!(parsed.watch_path, "var/prod/watch.txt");

        // An absolute bus carries its watch file with it, which is what a test or
        // a second instance needs.
        let absolute =
            Config::from_str(r#"{"env": "dev", "bus_path": "/tmp/somewhere/bus"}"#).expect("abs");
        assert_eq!(absolute.watch_path, "/tmp/somewhere/watch.txt");

        // A bus with no directory at all still gets a usable name.
        let bare = Config::from_str(r#"{"env": "dev", "bus_path": "bus"}"#).expect("bare");
        assert_eq!(bare.watch_path, "watch.txt");

        // An explicit path wins over the derivation.
        let explicit =
            Config::from_str(r#"{"env": "dev", "watch_path": "/tmp/w.txt"}"#).expect("explicit");
        assert_eq!(explicit.watch_path, "/tmp/w.txt");
    }

    #[test]
    fn the_ticker_catalog_path_has_a_default_and_can_be_replaced() {
        assert_eq!(
            Config::from_str(DEV).expect("dev").tickers_path,
            "config/tickers.txt"
        );
        let other =
            Config::from_str(r#"{"env": "dev", "tickers_path": "/tmp/mine.txt"}"#).expect("other");
        assert_eq!(other.tickers_path, "/tmp/mine.txt");
    }

    #[test]
    fn a_ring_capacity_that_is_not_a_power_of_two_is_refused() {
        let body = r#"{"env": "dev", "ring_capacity": 3000}"#;
        let err = Config::from_str(body).expect_err("must be refused");
        assert!(err.to_string().contains("power of two"), "{err}");
    }

    #[test]
    fn a_symbol_too_long_for_the_wire_field_is_refused() {
        let body = r#"{"env": "dev", "symbol": "AVERYLONGSYMBOL"}"#;
        let err = Config::from_str(body).expect_err("must be refused");
        assert!(err.to_string().contains("longer than"), "{err}");
    }

    #[test]
    fn pressure_thresholds_must_be_ordered() {
        let body = r#"{"env": "dev", "pressure_elevated_ns": 100, "pressure_saturated_ns": 50}"#;
        let err = Config::from_str(body).expect_err("must be refused");
        assert!(err.to_string().contains("below"), "{err}");
    }

    #[test]
    fn the_shipped_files_load_from_disk() {
        // Held because the prod branch below reads the environment twice: once to
        // decide what to expect and once inside the loader. Without the lock, a test
        // that sets the variable in between makes the two answers disagree.
        let _env = env_lock();
        // The test runs from the workspace root, where `config/` lives.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root");
        let dev = root.join("config/dev.json");
        if dev.is_file() {
            let c = Config::load(&dev).expect("shipped dev config");
            assert_eq!(c.env, "dev");
            assert!(!c.allow_orders);
        }
        let prod = root.join("config/prod.json");
        if prod.is_file() {
            // Refused without the environment variable, loadable with it.
            let allowed = std::env::var(ALLOW_PROD_ENV).as_deref() == Ok("1");
            assert_eq!(Config::load(&prod).is_ok(), allowed);
        }
    }
}
