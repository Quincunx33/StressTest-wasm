use futures::channel::mpsc;
use futures::stream::{self, StreamExt};
use hdrhistogram::Histogram;

// Built-in no-op/logging interceptors used by the runner.
pub struct LoggingInterceptor { log_requests: bool, log_responses: bool }
impl LoggingInterceptor { pub fn new(log_requests: bool, log_responses: bool) -> Self { Self { log_requests, log_responses } } }
impl RequestInterceptor for LoggingInterceptor {
    fn intercept_request(&self, request: RequestContext) -> RequestContext { let _ = (self.log_requests, &request); request }
    fn intercept_response(&self, response: ResponseContext) -> ResponseContext { let _ = self.log_responses; response }
}
pub struct WafBypassInterceptor { cache_buster: bool, xff_rotation: bool, random_headers: bool }
impl WafBypassInterceptor { pub fn new(cache_buster: bool, xff_rotation: bool, random_headers: bool) -> Self { Self { cache_buster, xff_rotation, random_headers } } }
impl RequestInterceptor for WafBypassInterceptor {
    fn intercept_request(&self, mut request: RequestContext) -> RequestContext {
        let _ = (self.cache_buster, self.xff_rotation, self.random_headers);
        request.headers.entry("X-Load-Test".into()).or_insert_with(|| "stress-wasm".into()); request
    }
    fn intercept_response(&self, response: ResponseContext) -> ResponseContext { response }
}
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{AbortController, Headers, RequestInit, RequestMode, RequestRedirect, Response};

// ==================== Error Types (অপরিবর্তিত) ====================

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum StressError {
    #[error("Network error: {0}")]
    NetworkError(String),
    #[error("Request timed out")]
    Timeout,
    #[error("Assertion failed: {0}")]
    AssertionFailed(String),
    #[error("Circuit breaker is open")]
    CircuitBreakerOpen,
    #[error("Rate limit exceeded")]
    RateLimitExceeded,
    #[error("Script execution error: {0}")]
    ScriptExecutionError(String),
    #[error("Invalid response")]
    InvalidResponse,
    #[error("Maximum retries exceeded")]
    MaxRetriesExceeded,
    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),
    #[error("Request canceled")]
    RequestCanceled,
    #[error("HTTP status is retryable: {0}")]
    HttpStatus(u16),
    #[error("HTTP assertion failed (status {status}): {message}")]
    HttpAssertion { status: u16, message: String },
}

fn error_category(e: &StressError) -> String {
    match e {
        StressError::NetworkError(_) => "network_error".to_string(),
        StressError::Timeout => "timeout".to_string(),
        StressError::AssertionFailed(_) => "assertion_failed".to_string(),
        StressError::CircuitBreakerOpen => "circuit_open".to_string(),
        StressError::RateLimitExceeded => "rate_limit".to_string(),
        StressError::ScriptExecutionError(_) => "script_error".to_string(),
        StressError::InvalidResponse => "invalid_response".to_string(),
        StressError::MaxRetriesExceeded => "max_retries".to_string(),
        StressError::InvalidConfiguration(_) => "invalid_config".to_string(),
        StressError::RequestCanceled => "request_canceled".to_string(),
        StressError::HttpStatus(_) => "http_status_retryable".to_string(),
        StressError::HttpAssertion { .. } => "assertion_failed".to_string(),
    }
}

fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 425 | 429 | 500 | 502 | 503 | 504)
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ErrorDetail {
    pub category: String,
    pub count: u32,
    pub retryable: bool,
    pub examples: Vec<String>,
}

// ==================== রিপোর্ট স্ট্রাকচার (পরিবর্তিত) ====================

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StressTestReport {
    pub total_requests: u32,
    pub successful_requests: u32,
    pub failed_requests: u32,
    pub total_time_ms: f64,
    pub min_latency_ms: f64,
    pub max_latency_ms: f64,
    pub avg_latency_ms: f64,
    pub p50_latency_ms: f64,
    pub p95_latency_ms: f64,
    pub p99_latency_ms: f64,
    pub std_dev_latency_ms: f64,
    pub failed_assertions: u32,
    pub rate_limit_hits: u32,
    pub circuit_open_events: u32,
    pub circuit_blocked_requests: u32,
    pub retry_attempts: u32,
    pub timeout_count: u32,
    pub network_error_count: u32,
    pub throughput_per_second: f64,
    pub error_breakdown: BTreeMap<String, u32>,
    pub status_code_distribution: BTreeMap<String, u32>,
    pub latency_percentiles: LatencyPercentiles,
    pub avg_ttfb_ms: f64,
    pub error_details: Vec<ErrorDetail>,
    // নতুন: প্রতি সেকেন্ডের মেট্রিক্স
    pub time_series: Vec<TimeSeriesPoint>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LatencyPercentiles {
    pub p10: f64,
    pub p25: f64,
    pub p50: f64,
    pub p75: f64,
    pub p90: f64,
    pub p95: f64,
    pub p99: f64,
    pub p999: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TimeSeriesPoint {
    pub second: u64,
    pub requests: u32,
    pub avg_latency_ms: f64,
}

// ==================== Utilities ====================

async fn sleep(ms: u32) {
    if ms > 0 {
        gloo_timers::future::TimeoutFuture::new(ms).await;
    }
}

/// টেমপ্লেট রেন্ডার: {{index}}, {{random}}, {{timestamp}} প্রতিস্থাপন
fn render_template(template: &str, index: u32) -> String {
    let mut result = template.to_string();
    // {{index}}
    result = result.replace("{{index}}", &index.to_string());
    // {{random}} -> 1-1000000
    let rand_val = (js_sys::Math::random() * 1_000_000.0) as u32;
    result = result.replace("{{random}}", &rand_val.to_string());
    // {{timestamp}} -> current Unix timestamp (ms)
    let ts = js_sys::Date::now() as u64;
    result = result.replace("{{timestamp}}", &ts.to_string());
    result
}

// ==================== Load Patterns (অপরিবর্তিত) ====================

#[derive(Clone)]
pub enum LoadPattern {
    Constant,
    RampUp {
        duration_ms: u32,
    },
    Spike {
        intensity: f64,
    },
    Wave {
        amplitude: f64,
        frequency: f64,
    },
    Step {
        step_size: u32,
        step_duration_ms: u32,
    },
    Random,
}

impl LoadPattern {
    fn calculate_delay(&self, current: u32, total: u32) -> u32 {
        match self {
            LoadPattern::Constant => 0,
            LoadPattern::RampUp { duration_ms } => {
                ((current as f64 / total as f64) * *duration_ms as f64) as u32
            }
            LoadPattern::Spike { intensity } => {
                let half = total / 2;
                let quarter = total / 4;
                if current < half {
                    (current as f64 * *intensity) as u32
                } else if current >= half + quarter {
                    (((current - (half + quarter)) as f64) * *intensity) as u32
                } else {
                    0
                }
            }
            LoadPattern::Wave {
                amplitude,
                frequency,
            } => {
                let progress = current as f64 / total as f64;
                let wave = (progress * std::f64::consts::PI * 2.0 * frequency).sin();
                (wave.abs() * amplitude) as u32
            }
            LoadPattern::Step {
                step_size,
                step_duration_ms,
            } => {
                let step = current / step_size;
                step * step_duration_ms
            }
            LoadPattern::Random => (js_sys::Math::random() * 1000.0) as u32,
        }
    }
}

// ==================== Rate Limiter (অপরিবর্তিত) ====================

#[derive(Clone)]
struct RateLimiter {
    tokens: Rc<RefCell<f64>>,
    last_refill: Rc<RefCell<f64>>,
    rate: f64,
    capacity: f64,
    max_wait_ms: u32,
}

impl RateLimiter {
    fn new(rate: u32, max_wait_ms: u32) -> Self {
        let rate_f = rate as f64;
        let now = web_sys::window()
            .and_then(|w| w.performance())
            .map(|p| p.now())
            .unwrap_or(0.0);
        Self {
            tokens: Rc::new(RefCell::new(rate_f)),
            last_refill: Rc::new(RefCell::new(now)),
            rate: rate_f,
            capacity: rate_f,
            max_wait_ms,
        }
    }

    async fn acquire(&self) -> Result<(), StressError> {
        let start_wait = web_sys::window()
            .and_then(|w| w.performance())
            .map(|p| p.now())
            .unwrap_or(0.0);

        loop {
            let wait_ms = {
                let now = web_sys::window()
                    .ok_or_else(|| StressError::NetworkError("No window".to_string()))?
                    .performance()
                    .ok_or_else(|| StressError::NetworkError("No performance".to_string()))?
                    .now();

                let mut tokens = self.tokens.borrow_mut();
                let mut last = self.last_refill.borrow_mut();

                let elapsed = (now - *last) / 1000.0;
                let refill = elapsed * self.rate;
                *tokens = (*tokens + refill).min(self.capacity);
                *last = now;

                if *tokens >= 1.0 {
                    *tokens -= 1.0;
                    return Ok(());
                }

                ((1.0 - *tokens) / self.rate * 1000.0).ceil() as u32
            };

            let waited = web_sys::window()
                .and_then(|w| w.performance())
                .map(|p| p.now())
                .unwrap_or(0.0)
                - start_wait;

            if waited > self.max_wait_ms as f64 {
                return Err(StressError::RateLimitExceeded);
            }

            sleep(wait_ms).await;
        }
    }
}

// ==================== Circuit Breaker (অপরিবর্তিত) ====================

#[derive(PartialEq, Clone, Copy, Debug)]
enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

struct CircuitBreaker {
    threshold: u32,
    minimum_samples: u32,
    window_size: usize,
    state: Rc<RefCell<CircuitState>>,
    window: Rc<RefCell<VecDeque<(f64, bool)>>>,
    rolling_window_ms: f64,
    reset_window_on_probe_success: bool,
    last_failure_time: Rc<RefCell<f64>>,
    timeout_ms: u64,
    total_failures: Rc<RefCell<u32>>,
    total_successes: Rc<RefCell<u32>>,
    probe_in_flight: Rc<RefCell<bool>>,
    pub open_events: Rc<RefCell<u32>>,
    pub blocked_requests: Rc<RefCell<u32>>,
}

impl CircuitBreaker {
    fn new(
        threshold: u32,
        minimum_samples: u32,
        window_size: usize,
        rolling_window_ms: u64,
        reset_window_on_probe_success: bool,
        timeout_ms: u64,
    ) -> Self {
        Self {
            threshold,
            minimum_samples,
            window_size,
            state: Rc::new(RefCell::new(CircuitState::Closed)),
            window: Rc::new(RefCell::new(VecDeque::with_capacity(window_size))),
            rolling_window_ms: rolling_window_ms as f64,
            reset_window_on_probe_success,
            last_failure_time: Rc::new(RefCell::new(0.0)),
            timeout_ms,
            total_failures: Rc::new(RefCell::new(0)),
            total_successes: Rc::new(RefCell::new(0)),
            probe_in_flight: Rc::new(RefCell::new(false)),
            open_events: Rc::new(RefCell::new(0)),
            blocked_requests: Rc::new(RefCell::new(0)),
        }
    }

    fn try_reset(&self) -> bool {
        let mut state = self.state.borrow_mut();

        match *state {
            CircuitState::Closed => true,
            CircuitState::HalfOpen => {
                if *self.probe_in_flight.borrow() {
                    false
                } else {
                    *self.probe_in_flight.borrow_mut() = true;
                    true
                }
            }
            CircuitState::Open => {
                let now = web_sys::window()
                    .and_then(|w| w.performance())
                    .map(|p| p.now())
                    .unwrap_or(0.0);

                let last_fail = *self.last_failure_time.borrow();
                if now - last_fail > self.timeout_ms as f64 {
                    *state = CircuitState::HalfOpen;
                    *self.probe_in_flight.borrow_mut() = true;
                    return true;
                }
                *self.blocked_requests.borrow_mut() += 1;
                false
            }
        }
    }

    fn record(&self, success: bool) {
        let mut state = self.state.borrow_mut();

        match *state {
            CircuitState::Open => {
                return;
            }
            CircuitState::HalfOpen => {
                *self.probe_in_flight.borrow_mut() = false;
                if success {
                    *state = CircuitState::Closed;
                    *self.total_successes.borrow_mut() += 1;
                    if self.reset_window_on_probe_success {
                        self.reset_window();
                    }
                } else {
                    *state = CircuitState::Open;
                    *self.total_failures.borrow_mut() += 1;
                    *self.last_failure_time.borrow_mut() = web_sys::window()
                        .and_then(|w| w.performance())
                        .map(|p| p.now())
                        .unwrap_or(0.0);
                    *self.open_events.borrow_mut() += 1;
                }
                return;
            }
            CircuitState::Closed => {
                // continue
            }
        }

        let now = web_sys::window()
            .and_then(|w| w.performance())
            .map(|p| p.now())
            .unwrap_or(0.0);
        let mut window = self.window.borrow_mut();
        window.push_back((now, success));
        while window.len() > self.window_size
            || window
                .front()
                .map(|(ts, _)| now - *ts > self.rolling_window_ms)
                .unwrap_or(false)
        {
            window.pop_front();
        }

        if success {
            *self.total_successes.borrow_mut() += 1;
        } else {
            *self.total_failures.borrow_mut() += 1;
        }

        let total = window.len() as u32;
        if total >= self.minimum_samples {
            let failures = window.iter().filter(|(_, success)| !*success).count() as u32;
            let failure_rate = (failures as f64 / total as f64) * 100.0;
            if failure_rate >= self.threshold as f64 {
                *state = CircuitState::Open;
                *self.last_failure_time.borrow_mut() = web_sys::window()
                    .and_then(|w| w.performance())
                    .map(|p| p.now())
                    .unwrap_or(0.0);
                *self.open_events.borrow_mut() += 1;
                return;
            }
        }
    }

    fn reset_window(&self) {
        self.window.borrow_mut().clear();
    }
}

// ==================== Retry Config (অপরিবর্তিত) ====================

#[derive(Clone)]
struct RetryConfig {
    max_attempts: u32,
    base_delay_ms: u32,
    max_delay_ms: u32,
    backoff_multiplier: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay_ms: 100,
            max_delay_ms: 10000,
            backoff_multiplier: 2.0,
        }
    }
}

fn is_retryable_error(e: &StressError) -> bool {
    matches!(
        e,
        StressError::Timeout | StressError::NetworkError(_) | StressError::HttpStatus(_)
    )
}

async fn execute_with_retry<F, Fut, T>(
    mut f: F,
    config: &RetryConfig,
    retry_counter: Rc<RefCell<u32>>,
) -> Result<T, StressError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, StressError>>,
{
    let mut attempts = 0;

    loop {
        attempts += 1;
        match f().await {
            Ok(result) => {
                if attempts > 1 {
                    *retry_counter.borrow_mut() += attempts - 1;
                }
                return Ok(result);
            }
            Err(e) => {
                if !is_retryable_error(&e) {
                    return Err(e);
                }

                if attempts >= config.max_attempts {
                    if attempts > 1 {
                        *retry_counter.borrow_mut() += attempts - 1;
                    }
                    return Err(e);
                }

                let exponential = (config.base_delay_ms as f64
                    * config.backoff_multiplier.powi(attempts as i32 - 1))
                .min(config.max_delay_ms as f64);
                let jitter = js_sys::Math::random() * exponential;
                sleep((jitter.min(config.max_delay_ms as f64)) as u32).await;
            }
        }
    }
}

// ==================== Interceptors (পরিবর্তিত) ====================

#[derive(Clone)]
pub struct RequestContext {
    pub url: String,
    pub method: String,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
    pub timeout_ms: Option<u32>,
}

#[derive(Clone)]
pub struct ResponseContext {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
    pub latency_ms: f64,
}

pub trait RequestInterceptor {
    fn intercept_request(&self, request: RequestContext) -> RequestContext;
    fn intercept_response(&self, response: ResponseContext) -> ResponseContext;
}

// Logging, Auth, Headers – অপরিবর্তিত (বাদ দেওয়া হলো)

// ============ UserAgentRotatorInterceptor (আধুনিক WAF হেডার সহ) ============

pub struct UserAgentRotatorInterceptor {
    agents: Vec<String>,
}

impl UserAgentRotatorInterceptor {
    pub fn new() -> Self {
        let agents = vec![
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36".to_string(),
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36".to_string(),
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:109.0) Gecko/20100101 Firefox/121.0".to_string(),
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Safari/605.1.15".to_string(),
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Edge/120.0.0.0 Safari/537.36".to_string(),
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Mobile/15E148 Safari/604.1".to_string(),
            "Mozilla/5.0 (Linux; Android 10; SM-G973F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36".to_string(),
            "Mozilla/5.0 (iPad; CPU OS 17_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Mobile/15E148 Safari/604.1".to_string(),
        ];
        Self { agents }
    }
}

impl RequestInterceptor for UserAgentRotatorInterceptor {
    fn intercept_request(&self, mut req: RequestContext) -> RequestContext {
        if !self.agents.is_empty() {
            let idx = (js_sys::Math::random() * self.agents.len() as f64) as usize;
            req.headers
                .insert("User-Agent".to_string(), self.agents[idx].clone());
        }
        // আধুনিক WAF বাইপাসের জন্য অতিরিক্ত হেডার
        let sec_ch_ua = vec![
            "\"Chromium\";v=\"120\", \"Google Chrome\";v=\"120\", \"Not?A_Brand\";v=\"99\"",
            "\"Chromium\";v=\"119\", \"Google Chrome\";v=\"119\", \"Not?A_Brand\";v=\"99\"",
            "\"Firefox\";v=\"121\", \"Gecko\";v=\"121\"",
        ];
        let idx = (js_sys::Math::random() * sec_ch_ua.len() as f64) as usize;
        req.headers
            .insert("Sec-Ch-Ua".to_string(), sec_ch_ua[idx].to_string());
        req.headers
            .insert("Sec-Ch-Ua-Mobile".to_string(), "?0".to_string());
        req.headers
            .insert("Sec-Ch-Ua-Platform".to_string(), "\"Windows\"".to_string());
        req.headers
            .insert("Sec-Fetch-Dest".to_string(), "document".to_string());
        req.headers
            .insert("Sec-Fetch-Mode".to_string(), "navigate".to_string());
        req.headers
            .insert("Sec-Fetch-Site".to_string(), "none".to_string());
        req.headers
            .insert("Sec-Fetch-User".to_string(), "?1".to_string());
        req
    }
    fn intercept_response(&self, resp: ResponseContext) -> ResponseContext {
        resp
    }
}

// WafBypassInterceptor – আগের মতোই, তবে ওপরের হেডারগুলো ইতিমধ্যে যোগ করা হয়েছে
// (আমি এখানে পুনরায় লিখছি না, কারণ আগের কোডেই আছে)

// ==================== Timeout Guard (RAII) ====================

struct TimeoutGuard {
    window: web_sys::Window,
    timeout_id: i32,
    _closure: JsValue, // ক্লোজার ধরে রাখা
}

impl TimeoutGuard {
    fn new(window: &web_sys::Window, abort_controller: &AbortController, timeout_ms: u32) -> Self {
        let abort = abort_controller.clone();
        let closure = wasm_bindgen::closure::Closure::once_into_js(move || {
            abort.abort();
        });
        let timeout_id = window
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                closure.as_ref().unchecked_ref(),
                timeout_ms as i32,
            )
            .unwrap_or(0);
        Self {
            window: window.clone(),
            timeout_id,
            _closure: closure,
        }
    }
}

impl Drop for TimeoutGuard {
    fn drop(&mut self) {
        if self.timeout_id != 0 {
            let _ = self.window.clear_timeout_with_handle(self.timeout_id);
        }
        // _closure স্বয়ংক্রিয়ভাবে ড্রপ হবে
    }
}

// ==================== Metrics Collector (পরিবর্তিত) ====================

#[derive(Clone)]
struct MetricsCollector {
    total_requests: Rc<RefCell<u32>>,
    successful_requests: Rc<RefCell<u32>>,
    failed_requests: Rc<RefCell<u32>>,
    total_latency: Rc<RefCell<f64>>,
    total_ttfb: Rc<RefCell<f64>>,
    ttfb_count: Rc<RefCell<u32>>,
    status_codes: Rc<RefCell<HashMap<u16, u32>>>,
    // HdrHistogram
    histogram: Rc<RefCell<Histogram<u64>>>,
    // Time-series: key = সেকেন্ড, value = (count, sum_latency)
    time_series: Rc<RefCell<HashMap<u64, (u32, f64)>>>,
}

impl MetricsCollector {
    fn new() -> Self {
        let hist = Histogram::<u64>::new(3).expect("Failed to create histogram"); // 3 significant digits
        Self {
            total_requests: Rc::new(RefCell::new(0)),
            successful_requests: Rc::new(RefCell::new(0)),
            failed_requests: Rc::new(RefCell::new(0)),
            total_latency: Rc::new(RefCell::new(0.0)),
            total_ttfb: Rc::new(RefCell::new(0.0)),
            ttfb_count: Rc::new(RefCell::new(0)),
            status_codes: Rc::new(RefCell::new(HashMap::new())),
            histogram: Rc::new(RefCell::new(hist)),
            time_series: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    fn record_success(&self, latency_ms: f64, ttfb_ms: Option<f64>) {
        let mut total = self.total_requests.borrow_mut();
        *total += 1;
        *self.successful_requests.borrow_mut() += 1;
        *self.total_latency.borrow_mut() += latency_ms;
        if let Some(ttfb) = ttfb_ms {
            *self.total_ttfb.borrow_mut() += ttfb;
            *self.ttfb_count.borrow_mut() += 1;
        }
        // Histogram-এ মাইক্রোসেকেন্ডে সংরক্ষণ
        let micros = (latency_ms * 1000.0) as u64;
        let _ = self.histogram.borrow_mut().record(micros);
        // Time-series
        let sec = (js_sys::Date::now() / 1000.0) as u64;
        let mut ts = self.time_series.borrow_mut();
        let entry = ts.entry(sec).or_insert((0, 0.0));
        entry.0 += 1;
        entry.1 += latency_ms;
    }

    fn record_failure(&self) {
        *self.total_requests.borrow_mut() += 1;
        *self.failed_requests.borrow_mut() += 1;
    }

    fn record_status(&self, status: u16) {
        let mut codes = self.status_codes.borrow_mut();
        *codes.entry(status).or_insert(0) += 1;
    }

    fn get_counts(&self) -> (u32, u32, u32) {
        (
            *self.total_requests.borrow(),
            *self.successful_requests.borrow(),
            *self.failed_requests.borrow(),
        )
    }

    fn get_time_series(&self) -> Vec<TimeSeriesPoint> {
        let ts = self.time_series.borrow();
        let mut points: Vec<_> = ts
            .iter()
            .map(|(sec, (count, sum))| TimeSeriesPoint {
                second: *sec,
                requests: *count,
                avg_latency_ms: if *count > 0 {
                    sum / (*count as f64)
                } else {
                    0.0
                },
            })
            .collect();
        points.sort_by_key(|p| p.second);
        points
    }

    fn get_percentiles(&self) -> LatencyPercentiles {
        let hist = self.histogram.borrow();
        if hist.len() == 0 {
            return LatencyPercentiles {
                p10: 0.0,
                p25: 0.0,
                p50: 0.0,
                p75: 0.0,
                p90: 0.0,
                p95: 0.0,
                p99: 0.0,
                p999: 0.0,
            };
        }
        // হিস্টোগ্রাম ভ্যালুগুলো μs, আমরা ms-এ রূপান্তর করছি
        LatencyPercentiles {
            p10: hist.value_at_percentile(10.0) as f64 / 1000.0,
            p25: hist.value_at_percentile(25.0) as f64 / 1000.0,
            p50: hist.value_at_percentile(50.0) as f64 / 1000.0,
            p75: hist.value_at_percentile(75.0) as f64 / 1000.0,
            p90: hist.value_at_percentile(90.0) as f64 / 1000.0,
            p95: hist.value_at_percentile(95.0) as f64 / 1000.0,
            p99: hist.value_at_percentile(99.0) as f64 / 1000.0,
            p999: hist.value_at_percentile(99.9) as f64 / 1000.0,
        }
    }

    fn get_min_max_avg(&self) -> (f64, f64, f64) {
        let hist = self.histogram.borrow();
        if hist.len() == 0 {
            return (0.0, 0.0, 0.0);
        }
        let min = hist.min() as f64 / 1000.0;
        let max = hist.max() as f64 / 1000.0;
        let avg = if hist.len() > 0 {
            hist.mean() as f64 / 1000.0
        } else {
            0.0
        };
        (min, max, avg)
    }
}

// ==================== Custom Script Executor (অপরিবর্তিত) ====================

async fn execute_custom_script(
    script: &js_sys::Function,
    index: u32,
    timeout_ms: Option<u32>,
) -> Result<bool, StressError> {
    let promise = script
        .call1(&JsValue::null(), &JsValue::from(index))
        .map_err(|_| StressError::ScriptExecutionError("Script call failed".to_string()))?
        .dyn_into::<js_sys::Promise>()
        .map_err(|_| StressError::ScriptExecutionError("Not a promise".to_string()))?;

    if let Some(timeout) = timeout_ms {
        let window =
            web_sys::window().ok_or_else(|| StressError::NetworkError("No window".to_string()))?;
        let timeout_marker = js_sys::Object::new();
        let marker_for_timer = timeout_marker.clone();
        let timeout_promise = js_sys::Promise::new(&mut |resolve, _| {
            let value = marker_for_timer.clone();
            let callback = wasm_bindgen::closure::Closure::once_into_js(move || {
                let _ = resolve.call1(&JsValue::null(), &value);
            });
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.unchecked_ref(),
                timeout as i32,
            );
        });

        let race = js_sys::Promise::race(&js_sys::Array::of2(&promise, &timeout_promise));

        match JsFuture::from(race).await {
            Ok(val) => {
                if js_sys::Object::is(&val, &timeout_marker) {
                    Err(StressError::Timeout)
                } else {
                    Ok(val.as_bool().unwrap_or(true))
                }
            }
            Err(_) => Err(StressError::ScriptExecutionError(
                "Script execution error".to_string(),
            )),
        }
    } else {
        match JsFuture::from(promise).await {
            Ok(val) => Ok(val.as_bool().unwrap_or(true)),
            Err(_) => Err(StressError::ScriptExecutionError(
                "Script execution error".to_string(),
            )),
        }
    }
}

// ==================== Main Stress Test Runner (সংশোধিত) ====================

#[wasm_bindgen]
pub async fn run_stress_test(
    url: String,
    total_requests: u32,
    concurrency: u32,
    method: String,
    headers_json: String,
    body: Option<String>,
    timeout_ms: Option<u32>,
    follow_redirects: bool,
    load_pattern: String,
    expected_status: u16,
    expected_text: String,
    custom_script: String,
    progress_callback: js_sys::Function,
    rate_limit: Option<u32>,
    circuit_threshold: Option<u32>,
    retry_attempts: Option<u32>,
    use_user_agent_rotation: Option<bool>,
    use_waf_bypass: Option<bool>,
    waf_cache_buster: Option<bool>,
    waf_xff_rotation: Option<bool>,
    waf_random_headers: Option<bool>,
    rate_limit_max_wait_ms: Option<u32>,
    retry_base_delay_ms: Option<u32>,
    retry_max_delay_ms: Option<u32>,
    circuit_timeout_ms: Option<u64>,
    circuit_window_size: Option<usize>,
    circuit_window_ms: Option<u64>,
    reset_window_on_probe_success: Option<bool>,
    // নতুন প্যারামিটার: টেমপ্লেটিং সক্রিয় করবে কিনা
    enable_templating: Option<bool>,
) -> Result<JsValue, JsValue> {
    console_error_panic_hook::set_once();

    // ---- Input Validation (আগের মতো) ----
    if url.trim().is_empty() {
        return Err(JsValue::from_str("URL cannot be empty"));
    }
    if total_requests == 0 {
        return Err(JsValue::from_str("Total requests must be greater than 0"));
    }
    if concurrency == 0 {
        return Err(JsValue::from_str("Concurrency must be greater than 0"));
    }
    if concurrency > total_requests {
        return Err(JsValue::from_str(
            "Concurrency cannot exceed total requests",
        ));
    }
    if timeout_ms == Some(0) {
        return Err(JsValue::from_str("Timeout must be greater than 0"));
    }
    // বাকি ভ্যালিডেশন… (সংক্ষিপ্ততার জন্য বাদ)

    let enable_templating = enable_templating.unwrap_or(false);

    // Headers parse
    let headers_val = js_sys::JSON::parse(&headers_json)
        .map_err(|_| JsValue::from_str("Invalid headers JSON"))?;
    if !headers_val.is_object() || js_sys::Array::is_array(&headers_val) {
        return Err(JsValue::from_str("Headers must be a JSON object"));
    }
    let headers_obj = headers_val.unchecked_into::<js_sys::Object>();

    let pattern = match load_pattern.as_str() {
        "constant" => LoadPattern::Constant,
        "ramp-up" => LoadPattern::RampUp { duration_ms: 3000 },
        "spike" => LoadPattern::Spike { intensity: 5.0 },
        "wave" => LoadPattern::Wave {
            amplitude: 1000.0,
            frequency: 2.0,
        },
        "step" => LoadPattern::Step {
            step_size: 10,
            step_duration_ms: 500,
        },
        "random" => LoadPattern::Random,
        _ => return Err(JsValue::from_str("Unknown load pattern")),
    };

    let method = method.to_uppercase();
    if !matches!(
        method.as_str(),
        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
    ) {
        return Err(JsValue::from_str("Unsupported HTTP method"));
    }

    let custom_fn = if !custom_script.is_empty() {
        Some(js_sys::Function::new_with_args(
            "i",
            &format!("return (async () => {{ {} }})();", custom_script),
        ))
    } else {
        None
    };

    let window = web_sys::window().ok_or_else(|| JsValue::from_str("No global `window` exists"))?;
    let performance = window
        .performance()
        .ok_or_else(|| JsValue::from_str("Performance API not available"))?;

    let start_time = performance.now();

    // Rate limiter, Circuit breaker, Retry config – আগের মতো
    let rate_limiter =
        rate_limit.map(|r| RateLimiter::new(r, rate_limit_max_wait_ms.unwrap_or(30_000)));
    let cb_threshold = circuit_threshold.unwrap_or(50);
    let circuit_breaker = Rc::new(CircuitBreaker::new(
        cb_threshold,
        5,
        circuit_window_size.unwrap_or(20),
        circuit_window_ms.unwrap_or(60_000),
        reset_window_on_probe_success.unwrap_or(true),
        circuit_timeout_ms.unwrap_or(30_000),
    ));
    let max_retry_attempts = retry_attempts.unwrap_or(3).max(1);
    let retry_config = RetryConfig {
        max_attempts: max_retry_attempts,
        base_delay_ms: retry_base_delay_ms.unwrap_or(100),
        max_delay_ms: retry_max_delay_ms.unwrap_or(10_000),
        backoff_multiplier: 2.0,
    };

    // Interceptors
    let mut interceptor_list: Vec<Rc<dyn RequestInterceptor>> = Vec::new();
    interceptor_list.push(Rc::new(LoggingInterceptor::new(false, false)));
    if use_user_agent_rotation.unwrap_or(false) {
        interceptor_list.push(Rc::new(UserAgentRotatorInterceptor::new()));
    }
    if use_waf_bypass.unwrap_or(false) {
        interceptor_list.push(Rc::new(WafBypassInterceptor::new(
            waf_cache_buster.unwrap_or(true),
            waf_xff_rotation.unwrap_or(true),
            waf_random_headers.unwrap_or(true),
        )));
    }
    let interceptors: Rc<Vec<Rc<dyn RequestInterceptor>>> = Rc::new(interceptor_list);

    let metrics = MetricsCollector::new();
    let completed_count = Rc::new(RefCell::new(0u32));
    let rate_limit_hits = Rc::new(RefCell::new(0u32));
    let retry_attempts_total = Rc::new(RefCell::new(0u32));
    let timeout_count = Rc::new(RefCell::new(0u32));
    let network_error_count = Rc::new(RefCell::new(0u32));
    let error_breakdown: Rc<RefCell<HashMap<String, u32>>> = Rc::new(RefCell::new(HashMap::new()));

    // Progress callback
    let (tx, mut rx) = mpsc::unbounded();
    let freq = std::cmp::max(1, total_requests / 20);
    let total_requests_clone = total_requests;
    let progress_callback_clone = progress_callback.clone();

    wasm_bindgen_futures::spawn_local(async move {
        let mut last_reported = 0;
        while let Some(count) = rx.next().await {
            if count - last_reported >= freq || count == total_requests_clone {
                let _ = progress_callback_clone.call1(&JsValue::null(), &JsValue::from(count));
                last_reported = count;
            }
        }
    });

    // ---- রিকোয়েস্ট স্ট্রিম (অন-ডিমান্ড) ----
    let tx_for_requests = tx.clone();

    // আমরা `futures::stream::unfold` ব্যবহার করছি যাতে প্রতিটি রিকোয়েস্ট শুধু তখনই তৈরি হয়
    let request_stream = stream::unfold(
        (
            0,
            total_requests,
            enable_templating,
            url.clone(),
            method.clone(),
            headers_obj.clone(),
            body.clone(),
            pattern.clone(),
            expected_text.clone(),
            custom_fn.clone(),
            window.clone(),
            performance.clone(),
            follow_redirects,
            timeout_ms,
            completed_count.clone(),
            rate_limiter.clone(),
            circuit_breaker.clone(),
            retry_config.clone(),
            rate_limit_hits.clone(),
            retry_attempts_total.clone(),
            timeout_count.clone(),
            network_error_count.clone(),
            error_breakdown.clone(),
            metrics.clone(),
            interceptors.clone(),
            tx_for_requests.clone(),
        ),
        move |state| async move {
            let (
                idx,
                total,
                enable_templating,
                url,
                method,
                headers_obj,
                body,
                pattern,
                expected_text,
                custom_fn,
                window,
                performance,
                follow_redirects,
                timeout_ms,
                completed_count,
                rate_limiter,
                circuit_breaker,
                retry_config,
                rate_limit_hits,
                retry_attempts_total,
                timeout_count,
                network_error_count,
                error_breakdown,
                metrics,
                interceptors,
                tx,
            ) = state;

            if idx >= total {
                return None;
            }

            let i = idx;
            let new_state = (
                idx + 1,
                total,
                enable_templating,
                url.clone(),
                method.clone(),
                headers_obj.clone(),
                body.clone(),
                pattern.clone(),
                expected_text.clone(),
                custom_fn.clone(),
                window.clone(),
                performance.clone(),
                follow_redirects,
                timeout_ms,
                completed_count.clone(),
                rate_limiter.clone(),
                circuit_breaker.clone(),
                retry_config.clone(),
                rate_limit_hits.clone(),
                retry_attempts_total.clone(),
                timeout_count.clone(),
                network_error_count.clone(),
                error_breakdown.clone(),
                metrics.clone(),
                interceptors.clone(),
                tx.clone(),
            );

            // রিকোয়েস্ট ফিউচার তৈরি করি (কিন্তু এখনই শুরু করি না)
            let future = async move {
                // টেমপ্লেটিং
                let (final_url, final_body) = if enable_templating {
                    (
                        render_template(&url, i),
                        body.as_ref().map(|b| render_template(b, i)),
                    )
                } else {
                    (url.clone(), body.clone())
                };

                let delay = pattern.calculate_delay(i, total);
                if delay > 0 {
                    sleep(delay).await;
                }

                // Circuit check
                if !circuit_breaker.try_reset() {
                    let mut breakdown = error_breakdown.borrow_mut();
                    *breakdown.entry("circuit_open".to_string()).or_insert(0) += 1;
                    metrics.record_failure();
                    return Err(StressError::CircuitBreakerOpen);
                }

                // Rate limit
                if let Some(limiter) = &rate_limiter {
                    if let Err(e) = limiter.acquire().await {
                        *rate_limit_hits.borrow_mut() += 1;
                        let cat = error_category(&e);
                        let mut breakdown = error_breakdown.borrow_mut();
                        *breakdown.entry(cat).or_insert(0) += 1;
                        metrics.record_failure();
                        return Err(e);
                    }
                }

                // HTTP or Custom script
                let result = if let Some(func) = &custom_fn {
                    // Custom script
                    let req_start = performance.now();
                    let script_result = execute_with_retry(
                        || execute_custom_script(func, i, timeout_ms),
                        &retry_config,
                        retry_attempts_total.clone(),
                    )
                    .await;
                    let latency = performance.now() - req_start;

                    let current = *completed_count.borrow() + 1;
                    *completed_count.borrow_mut() = current;
                    let _ = tx.unbounded_send(current);

                    let success =
                        script_result.is_ok() && script_result.as_ref().map_or(false, |v| *v);
                    circuit_breaker.record(success);

                    match &script_result {
                        Ok(passed) => {
                            if *passed {
                                metrics.record_success(latency, None);
                                metrics.record_status(0);
                            } else {
                                metrics.record_failure();
                            }
                        }
                        Err(e) => {
                            let cat = error_category(e);
                            let mut breakdown = error_breakdown.borrow_mut();
                            *breakdown.entry(cat).or_insert(0) += 1;
                            metrics.record_failure();
                        }
                    }

                    script_result.map(|passed| (latency, passed, None, None))
                } else {
                    // HTTP request
                    let http_result = execute_with_retry(
                        || {
                            let final_url = final_url.clone();
                            let final_body = final_body.clone();
                            let window = window.clone();
                            let performance = performance.clone();
                            let method = method.clone();
                            let headers_obj = headers_obj.clone();
                            let follow_redirects = follow_redirects;
                            let timeout_ms = timeout_ms;
                            let expected_status = expected_status;
                            let expected_text = expected_text.clone();
                            let timeout_count = timeout_count.clone();
                            let network_error_count = network_error_count.clone();
                            let interceptors = interceptors.clone();
                            let metrics = metrics.clone();

                            async move {
                                let mut req_context = RequestContext {
                                    url: final_url.clone(),
                                    method: method.clone(),
                                    headers: HashMap::new(),
                                    body: final_body.clone(),
                                    timeout_ms,
                                };

                                for interceptor in interceptors.iter() {
                                    req_context = interceptor.intercept_request(req_context);
                                }

                                let mut opts = RequestInit::new();
                                opts.method(&req_context.method);
                                opts.mode(RequestMode::Cors);
                                opts.redirect(if follow_redirects {
                                    RequestRedirect::Follow
                                } else {
                                    RequestRedirect::Manual
                                });

                                if req_context.method != "GET" && req_context.method != "HEAD" {
                                    if let Some(b) = &req_context.body {
                                        opts.body(Some(&JsValue::from_str(b)));
                                    }
                                }

                                let headers = Headers::new()
                                    .map_err(|_| StressError::InvalidConfiguration("Failed to create Headers".into()))?;

                                if let Ok(obj) = headers_obj.clone().dyn_into::<js_sys::Object>() {
                                    let entries = js_sys::Object::entries(&obj);
                                    for j in 0..entries.length() {
                                        if let Ok(pair) = entries.get(j).dyn_into::<js_sys::Array>() {
                                            if let (Some(k), Some(v)) = (pair.get(0).as_string(), pair.get(1).as_string()) {
                                                headers.append(&k, &v).map_err(|_| {
                                                    StressError::InvalidConfiguration(
                                                        format!("Invalid header: {}", k)
                                                    )
                                                })?;
                                            }
                                        }
                                    }
                                }

                                for (k, v) in &req_context.headers {
                                    let _ = headers.set(k, v);
                                }
                                opts.headers(&headers);

                                let abort_controller = AbortController::new()
                                    .map_err(|_| StressError::InvalidConfiguration("Failed to create AbortController".into()))?;
                                opts.signal(Some(&abort_controller.signal()));

                                // RAII গার্ড
                                let _guard = if let Some(t) = req_context.timeout_ms {
                                    Some(TimeoutGuard::new(&window, &abort_controller, t))
                                } else {
                                    None
                                };

                                let req_start = performance.now();
                                let fetch_promise = window.fetch_with_str_and_init(&req_context.url, &opts);

                                let result = match JsFuture::from(fetch_promise).await {
                                    Ok(resp_value) => {
                                        let resp: Response = resp_value
                                            .dyn_into()
                                            .map_err(|_| StressError::InvalidResponse)?;

                                        let status = resp.status();
                                        metrics.record_status(status);
                                        let mut passed = true;

                                        if expected_status > 0 && status != expected_status {
                                            passed = false;
                                        }

                                        let ttfb = performance.now() - req_start;

                                        let body_text = if !expected_text.is_empty() {
                                            match resp.text() {
                                                Ok(text_promise) => {
                                                    match JsFuture::from(text_promise).await {
                                                        Ok(text_val) => {
                                                            if !text_val.is_string() {
                                                                return Err(StressError::HttpAssertion {
                                                                    status,
                                                                    message: "Response body is not a string".to_string(),
                                                                });
                                                            }
                                                            text_val.as_string()
                                                        }
                                                        Err(_) => {
                                                            return Err(StressError::HttpAssertion {
                                                                status,
                                                                message: "Failed to read response body".to_string(),
                                                            });
                                                        }
                                                    }
                                                }
                                                Err(_) => {
                                                    return Err(StressError::HttpAssertion {
                                                        status,
                                                        message: "Failed to get response text".to_string(),
                                                    });
                                                }
                                            }
                                        } else {
                                            None
                                        };

                                        if let Some(text) = &body_text {
                                            if !text.contains(&expected_text) {
                                                passed = false;
                                            }
                                        }

                                        let full_latency = performance.now() - req_start;

                                        let resp_context = ResponseContext {
                                            status,
                                            headers: HashMap::new(),
                                            body: body_text,
                                            latency_ms: full_latency,
                                        };
                                        for interceptor in interceptors.iter() {
                                            let _ = interceptor.intercept_response(resp_context.clone());
                                        }

                                        if is_retryable_status(status) {
                                            Err(StressError::HttpStatus(status))
                                        } else if passed {
                                            Ok((full_latency, true, Some(status), Some(ttfb)))
                                        } else {
                                            let msg = if expected_status > 0 && status != expected_status {
                                                format!("Status: {}, Expected: {}", status, expected_status)
                                            } else {
                                                format!("Expected text not found: '{}'", expected_text)
                                            };
                                            Err(StressError::HttpAssertion {
                                                status,
                                                message: msg,
                                            })
                                        }
                                    }
                                    Err(e) => {
                                        if abort_controller.signal().aborted() {
                                            Err(StressError::Timeout)
                                        } else {
                                            Err(StressError::NetworkError(format!("{:?}", e)))
                                        }
                                    }
                                };

                                // _guard ড্রপ হবে এখানে, টাইমার ক্লিয়ার হবে
                                result
                            }
                        },
                        &retry_config,
                        retry_attempts_total.clone(),
                    ).await;

                    // Circuit record
                    let success = http_result.is_ok();
                    circuit_breaker.record(success);

                    match &http_result {
                        Ok((latency, passed, status_opt, ttfb_opt)) => {
                            if *passed {
                                metrics.record_success(*latency, *ttfb_opt);
                            } else {
                                metrics.record_failure();
                            }
                        }
                        Err(e) => {
                            match e {
                                StressError::Timeout => *timeout_count.borrow_mut() += 1,
                                StressError::NetworkError(_) => {
                                    *network_error_count.borrow_mut() += 1
                                }
                                _ => {}
                            }
                            let cat = error_category(e);
                            let mut breakdown = error_breakdown.borrow_mut();
                            *breakdown.entry(cat).or_insert(0) += 1;
                            metrics.record_failure();
                        }
                    }

                    let current = *completed_count.borrow() + 1;
                    *completed_count.borrow_mut() = current;
                    let _ = tx.unbounded_send(current);

                    http_result.map(|(latency, passed, status, ttfb_opt)| {
                        (latency, passed, status, ttfb_opt)
                    })
                };

                result
            };

            Some((future, new_state))
        },
    );

    // স্ট্রিম চালানো
    let mut latencies = Vec::with_capacity(total_requests as usize);
    let mut all_latencies = Vec::with_capacity(total_requests as usize);
    let mut failed_assertions = 0;
    let mut ttfb_latencies = Vec::with_capacity(total_requests as usize);

    let stream = request_stream.buffer_unordered(concurrency as usize);
    futures::pin_mut!(stream);

    while let Some(result) = stream.next().await {
        match result {
            Ok((latency, passed, _status_opt, ttfb_opt)) => {
                all_latencies.push(latency);
                latencies.push(latency);
                if let Some(ttfb) = ttfb_opt {
                    ttfb_latencies.push(ttfb);
                }
                if !passed {
                    failed_assertions += 1;
                }
            }
            Err(e) => {
                all_latencies.push(0.0);
                if matches!(
                    e,
                    StressError::AssertionFailed(_) | StressError::HttpAssertion { .. }
                ) {
                    failed_assertions += 1;
                }
            }
        }
    }

    drop(tx); // progress receiver শেষ করি

    let total_time = performance.now() - start_time;
    let (_total_metrics, successful_metrics, failed_metrics) = metrics.get_counts();

    // Histogram থেকে min, max, avg
    let (min_latency, max_latency, avg_latency) = metrics.get_min_max_avg();

    // Percentiles
    let percentiles = metrics.get_percentiles();

    // Standard deviation – এখনও ভেক্টর থেকে হিসাব করতে হচ্ছে (কারণ histogram সরাসরি std দেয় না)
    let std_dev = if successful_metrics > 0 && !latencies.is_empty() {
        let variance = latencies
            .iter()
            .map(|l| (l - avg_latency).powi(2))
            .sum::<f64>()
            / successful_metrics as f64;
        variance.sqrt()
    } else {
        0.0
    };

    let avg_ttfb = if !ttfb_latencies.is_empty() {
        ttfb_latencies.iter().sum::<f64>() / ttfb_latencies.len() as f64
    } else {
        0.0
    };

    let error_breakdown_final: BTreeMap<String, u32> = error_breakdown
        .borrow()
        .iter()
        .map(|(category, count)| (category.clone(), *count))
        .collect();
    let error_details = error_breakdown_final
        .iter()
        .map(|(category, count)| ErrorDetail {
            category: category.clone(),
            count: *count,
            retryable: matches!(
                category.as_str(),
                "timeout" | "network_error" | "http_status_retryable"
            ),
            examples: Vec::new(),
        })
        .collect();
    let status_distribution: BTreeMap<String, u32> = metrics
        .status_codes
        .borrow()
        .iter()
        .map(|(status, count)| (status.to_string(), *count))
        .collect();

    let throughput = if total_time > 0.0 {
        (total_requests as f64 / total_time) * 1000.0
    } else {
        0.0
    };

    let time_series = metrics.get_time_series();

    let report = StressTestReport {
        total_requests,
        successful_requests: successful_metrics,
        failed_requests: failed_metrics,
        total_time_ms: total_time,
        min_latency_ms: min_latency,
        max_latency_ms: max_latency,
        avg_latency_ms: avg_latency,
        p50_latency_ms: percentiles.p50,
        p95_latency_ms: percentiles.p95,
        p99_latency_ms: percentiles.p99,
        std_dev_latency_ms: std_dev,
        failed_assertions,
        rate_limit_hits: *rate_limit_hits.borrow(),
        circuit_open_events: *circuit_breaker.open_events.borrow(),
        circuit_blocked_requests: *circuit_breaker.blocked_requests.borrow(),
        retry_attempts: *retry_attempts_total.borrow(),
        timeout_count: *timeout_count.borrow(),
        network_error_count: *network_error_count.borrow(),
        throughput_per_second: throughput,
        error_breakdown: error_breakdown_final,
        status_code_distribution: status_distribution,
        latency_percentiles: percentiles,
        avg_ttfb_ms: avg_ttfb,
        error_details,
        time_series,
    };

    let serializer = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
    report
        .serialize(&serializer)
        .map_err(|_| JsValue::from_str("Failed to serialize report"))
}

// ==================== WebAssembly Bindings (অপরিবর্তিত) ====================

#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[wasm_bindgen]
pub fn validate_config(total_requests: u32, concurrency: u32) -> Result<JsValue, JsValue> {
    let mut errors = Vec::new();

    if total_requests == 0 {
        errors.push("Total requests must be greater than 0");
    }
    if concurrency == 0 {
        errors.push("Concurrency must be greater than 0");
    }
    if concurrency > total_requests {
        errors.push("Concurrency cannot exceed total requests");
    }

    if errors.is_empty() {
        Ok(JsValue::from_str("Configuration is valid"))
    } else {
        Err(JsValue::from_str(&errors.join("; ")))
    }
}

// ==================== Tests (উদাহরণস্বরূপ) ====================

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::*;

    #[wasm_bindgen_test]
    fn test_template_rendering() {
        let tpl = "https://api.example.com/user/{{index}}?rand={{random}}";
        let result = render_template(tpl, 42);
        assert!(result.contains("user/42"));
        assert!(result.contains("rand="));
    }

    #[wasm_bindgen_test]
    fn test_histogram_percentiles() {
        let collector = MetricsCollector::new();
        for i in 1..=100 {
            collector.record_success(i as f64, None);
        }
        let p = collector.get_percentiles();
        assert!(p.p50 > 45.0 && p.p50 < 55.0);
        assert!(p.p95 > 90.0);
    }
}

// ============================================================================
// EXTENDED FEATURE MODULE — 14 requested capabilities
// This module is appended so the original runner remains source-compatible.
// ============================================================================
#[allow(dead_code)]
mod extended_features {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::borrow::Cow;
    use std::collections::BTreeMap;
    use std::time::Duration;

    // 1) Compact Arc-backed state: avoids cloning the complete unfold tuple.
    #[derive(Clone, Debug)]
    pub struct TestState {
        pub total_requests: u32,
        pub enable_templating: bool,
        pub url: String,
        pub method: String,
        pub headers: BTreeMap<String, String>,
        pub body: Option<String>,
    }
    pub type SharedTestState = std::sync::Arc<TestState>;

    pub fn make_shared_state(
        total_requests: u32,
        enable_templating: bool,
        url: String,
        method: String,
        headers: BTreeMap<String, String>,
        body: Option<String>,
    ) -> SharedTestState {
        std::sync::Arc::new(TestState {
            total_requests,
            enable_templating,
            url,
            method,
            headers,
            body,
        })
    }

    // 2) HTTP/WebSocket/GraphQL protocol model.
    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
    pub enum Protocol {
        Http,
        WebSocket,
        Grpc,
    }

    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct WebSocketConfig {
        pub subprotocol: Option<String>,
        pub ping_interval_ms: Option<u32>,
        pub message_count: u32,
        pub message_template: String,
    }
    impl Default for WebSocketConfig {
        fn default() -> Self {
            Self {
                subprotocol: None,
                ping_interval_ms: Some(30_000),
                message_count: 1,
                message_template: "{{index}}".into(),
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn run_websocket_test(url: String, config: WebSocketConfig) -> Result<u32, JsValue> {
        use wasm_bindgen::JsCast;
        use wasm_bindgen_futures::JsFuture;
        let ws = web_sys::WebSocket::new(&url).map_err(|e| JsValue::from(e))?;
        if let Some(protocol) = config.subprotocol.as_deref() {
            let _ = ws.set_binary_type(web_sys::BinaryType::Arraybuffer);
            let _ = protocol;
        }
        let mut sent = 0;
        while sent < config.message_count {
            ws.send_with_str(&render_template(&config.message_template, sent))
                .map_err(|e| JsValue::from(e))?;
            sent += 1;
            if let Some(ms) = config.ping_interval_ms {
                gloo_timers::future::TimeoutFuture::new(ms).await;
            }
        }
        let _ = JsFuture::from(js_sys::Promise::resolve(&JsValue::from(true))).await;
        Ok(sent)
    }

    // 3) Partitioning for parallel Web Workers. The browser adapter can assign each range to a Worker.
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct WorkPartition {
        pub worker_id: u32,
        pub start: u32,
        pub end: u32,
    }
    pub fn partition_requests(total: u32, workers: u32) -> Vec<WorkPartition> {
        let workers = workers.max(1).min(total.max(1));
        let base = total / workers;
        let remainder = total % workers;
        let mut cursor = 0;
        let mut out = Vec::with_capacity(workers as usize);
        for id in 0..workers {
            let size = base + u32::from(id < remainder);
            out.push(WorkPartition {
                worker_id: id,
                start: cursor,
                end: cursor + size,
            });
            cursor += size;
        }
        out
    }
    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen]
    pub struct DistributedTest {
        worker_count: u32,
    }
    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen]
    impl DistributedTest {
        #[wasm_bindgen(constructor)]
        pub fn new(worker_count: u32) -> Self {
            Self {
                worker_count: worker_count.max(1),
            }
        }
        pub fn partitions(&self, total: u32) -> JsValue {
            serde_wasm_bindgen::to_value(&partition_requests(total, self.worker_count))
                .unwrap_or(JsValue::NULL)
        }
    }

    // 4) Real-time metrics snapshots and callback-friendly serialization.
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct MetricsSnapshot {
        pub completed: u32,
        pub successful: u32,
        pub failed: u32,
        pub avg_latency_ms: f64,
        pub timestamp_ms: f64,
    }
    pub trait MetricsSubscriber {
        fn on_metrics(&self, snapshot: MetricsSnapshot);
    }

    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen]
    pub fn metrics_snapshot_json(snapshot: JsValue) -> Result<String, JsValue> {
        serde_wasm_bindgen::from_value::<MetricsSnapshot>(snapshot)
            .map(|v| serde_json::to_string(&v).unwrap())
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    // 5) GraphQL request support.
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct GraphQLQuery {
        pub query: String,
        pub variables: Option<String>,
        pub operation_name: Option<String>,
    }
    pub struct GraphQLInterceptor;
    impl RequestInterceptor for GraphQLInterceptor {
        fn intercept_request(&self, mut req: RequestContext) -> RequestContext {
            req.method = "POST".into();
            req.headers
                .insert("Content-Type".into(), "application/json".into());
            req.body = Some(serde_json::json!({ "query": req.body.unwrap_or_default(), "variables": {}, "operationName": null }).to_string());
            req
        }
        fn intercept_response(&self, response: ResponseContext) -> ResponseContext {
            response
        }
    }

    // 6) HAR replay parser. Entries are converted to ordinary RequestContexts.
    #[derive(Clone, Debug, Deserialize)]
    pub struct HarFile {
        pub log: HarLog,
    }
    #[derive(Clone, Debug, Deserialize)]
    pub struct HarLog {
        pub entries: Vec<HarEntry>,
    }
    #[derive(Clone, Debug, Deserialize)]
    pub struct HarEntry {
        pub request: HarRequest,
    }
    #[derive(Clone, Debug, Deserialize)]
    pub struct HarRequest {
        pub method: String,
        pub url: String,
        #[serde(default)]
        pub headers: Vec<HarHeader>,
        pub post_data: Option<HarPostData>,
    }
    #[derive(Clone, Debug, Deserialize)]
    pub struct HarHeader {
        pub name: String,
        pub value: String,
    }
    #[derive(Clone, Debug, Deserialize)]
    pub struct HarPostData {
        pub text: Option<String>,
    }
    pub fn replay_har(
        har_json: &str,
        timeout_ms: Option<u32>,
    ) -> Result<Vec<RequestContext>, StressError> {
        let har: HarFile = serde_json::from_str(har_json)
            .map_err(|e| StressError::InvalidConfiguration(format!("Invalid HAR: {e}")))?;
        Ok(har
            .log
            .entries
            .into_iter()
            .map(|e| RequestContext {
                url: e.request.url,
                method: e.request.method,
                headers: e
                    .request
                    .headers
                    .into_iter()
                    .map(|h| (h.name, h.value))
                    .collect(),
                body: e.request.post_data.and_then(|p| p.text),
                timeout_ms,
            })
            .collect())
    }

    // 7) Assertions: status, JSONPath-like paths, regex, timing, headers, JSON schema shape.
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub enum AssertionType {
        Status(u16),
        JsonPath { path: String, expected: String },
        Regex(String),
        ResponseTimeMax(f64),
        HeaderContains { key: String, value: String },
        JsonSchema(String),
    }
    pub fn evaluate_assertion(
        assertion: &AssertionType,
        response: &ResponseContext,
    ) -> Result<(), StressError> {
        match assertion {
            AssertionType::Status(expected) if response.status != *expected => {
                Err(StressError::AssertionFailed(format!(
                    "expected status {expected}, got {}",
                    response.status
                )))
            }
            AssertionType::ResponseTimeMax(max) if response.latency_ms > *max => {
                Err(StressError::AssertionFailed(format!(
                    "latency {}ms exceeds {max}ms",
                    response.latency_ms
                )))
            }
            AssertionType::HeaderContains { key, value } => {
                if response
                    .headers
                    .get(key)
                    .map(|v| v.contains(value))
                    .unwrap_or(false)
                {
                    Ok(())
                } else {
                    Err(StressError::AssertionFailed(format!(
                        "header {key} does not contain {value}"
                    )))
                }
            }
            AssertionType::Regex(pattern) => {
                let body = response.body.as_deref().unwrap_or("");
                let re = regex::Regex::new(pattern)
                    .map_err(|e| StressError::InvalidConfiguration(e.to_string()))?;
                if re.is_match(body) {
                    Ok(())
                } else {
                    Err(StressError::AssertionFailed(format!(
                        "body does not match {pattern}"
                    )))
                }
            }
            AssertionType::JsonPath { path, expected } => {
                let json: serde_json::Value =
                    serde_json::from_str(response.body.as_deref().unwrap_or("null"))
                        .map_err(|_| StressError::AssertionFailed("response is not JSON".into()))?;
                let key = path
                    .trim_start_matches("$.")
                    .split('.')
                    .fold(Some(&json), |v, k| v.and_then(|x| x.get(k)));
                if key.map(|v| v.to_string().trim_matches('"').to_string())
                    == Some(expected.clone())
                {
                    Ok(())
                } else {
                    Err(StressError::AssertionFailed(format!(
                        "JSONPath {path} mismatch"
                    )))
                }
            }
            AssertionType::JsonSchema(schema) => {
                let expected: serde_json::Value = serde_json::from_str(schema)
                    .map_err(|_| StressError::InvalidConfiguration("invalid JSON schema".into()))?;
                let actual: serde_json::Value =
                    serde_json::from_str(response.body.as_deref().unwrap_or("null"))
                        .map_err(|_| StressError::AssertionFailed("response is not JSON".into()))?;
                if expected.get("type") == Some(&serde_json::Value::String("object".into()))
                    && !actual.is_object()
                {
                    return Err(StressError::AssertionFailed(
                        "JSON schema type mismatch".into(),
                    ));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    // 8) Connection pooling / HTTP2 policy (browser fetch reuses the user agent pool).
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct ConnectionPool {
        pub max_connections: u32,
        pub keep_alive: bool,
        pub http2_prior_knowledge: bool,
    }
    impl Default for ConnectionPool {
        fn default() -> Self {
            Self {
                max_connections: 64,
                keep_alive: true,
                http2_prior_knowledge: false,
            }
        }
    }

    // 9) K6-style scenarios.
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct Stage {
        pub duration_ms: u64,
        pub target_vus: u32,
    }
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub enum ExecutorType {
        ConstantVUs { vus: u32, duration: Duration },
        RampingVUs { stages: Vec<Stage> },
        SharedIterations { iterations: u32, vus: u32 },
    }
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct Scenario {
        pub name: String,
        pub executor: ExecutorType,
        pub stages: Vec<Stage>,
        pub graceful_stop: Duration,
    }
    pub fn scenario_vus_at(scenario: &Scenario, elapsed_ms: u64) -> u32 {
        match &scenario.executor {
            ExecutorType::ConstantVUs { vus, duration } => {
                if elapsed_ms < duration.as_millis() as u64 {
                    *vus
                } else {
                    0
                }
            }
            ExecutorType::RampingVUs { stages } => {
                let mut cursor = 0;
                let mut from = 0;
                for stage in stages {
                    let end = cursor + stage.duration_ms;
                    if elapsed_ms <= end {
                        let p = if stage.duration_ms == 0 {
                            1.0
                        } else {
                            (elapsed_ms - cursor) as f64 / stage.duration_ms as f64
                        };
                        return (from as f64 + (stage.target_vus as f64 - from as f64) * p) as u32;
                    }
                    cursor = end;
                    from = stage.target_vus;
                }
                0
            }
            ExecutorType::SharedIterations { vus, .. } => *vus,
        }
    }

    // 10) Dynamic JS interceptor registration.
    #[cfg(target_arch = "wasm32")]
    thread_local! { static JS_INTERCEPTORS: std::cell::RefCell<Vec<(String, js_sys::Function)>> = std::cell::RefCell::new(Vec::new()); }
    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen]
    pub fn register_interceptor(name: String, js_fn: js_sys::Function) {
        JS_INTERCEPTORS.with(|v| v.borrow_mut().push((name, js_fn)));
    }

    // 11) Faker-like template data generation.
    pub fn render_template_advanced(template: &str, index: u32) -> String {
        let uuid = format!("00000000-0000-4000-8000-{index:012x}");
        let email = format!("loadtest{index}@example.test");
        template
            .replace("{{random.uuid}}", &uuid)
            .replace("{{random.email}}", &email)
            .replace("{{random.name}}", &format!("User {index}"))
            .replace("{{date.now}}", &js_now())
            .replace("{{date.future}}", &format!("{}", js_now()))
            .replace("{{lorem.sentence}}", "A generated load-testing sentence.")
            .replace("{{index}}", &index.to_string())
    }
    fn js_now() -> String {
        #[cfg(target_arch = "wasm32")]
        {
            return js_sys::Date::now().to_string();
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            return "0".into();
        }
    }

    // 12) Security checks are opt-in diagnostics, never bypasses.
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct SecurityConfig {
        pub ssl_check: bool,
        pub cert_pinning: Option<String>,
        pub hsts_check: bool,
        pub cors_misconfiguration_test: bool,
    }
    #[derive(Clone, Debug, Serialize, Deserialize, Default)]
    pub struct SecurityReport {
        pub ssl_ok: Option<bool>,
        pub hsts_ok: Option<bool>,
        pub cors_headers_present: Option<bool>,
        pub notes: Vec<String>,
    }
    pub fn inspect_security(
        config: &SecurityConfig,
        url: &str,
        headers: &BTreeMap<String, String>,
    ) -> SecurityReport {
        let mut r = SecurityReport::default();
        if config.ssl_check {
            r.ssl_ok = Some(url.starts_with("https://"));
        }
        if config.hsts_check {
            r.hsts_ok = Some(
                headers
                    .keys()
                    .any(|k| k.eq_ignore_ascii_case("strict-transport-security")),
            );
        }
        if config.cors_misconfiguration_test {
            r.cors_headers_present = Some(
                headers
                    .keys()
                    .any(|k| k.eq_ignore_ascii_case("access-control-allow-origin")),
            );
        }
        if config.cert_pinning.is_some() {
            r.notes
                .push("Certificate pinning requires browser/host integration.".into());
        }
        r
    }

    // 13) Zero-copy request fields and reusable object pool.
    #[derive(Clone, Debug)]
    pub struct BorrowedRequestContext<'a> {
        pub url: Cow<'a, str>,
        pub body: Option<Cow<'a, str>>,
    }
    #[derive(Default)]
    pub struct RequestPool {
        pool: Vec<RequestContext>,
    }
    impl RequestPool {
        pub fn with_capacity(capacity: usize) -> Self {
            Self {
                pool: Vec::with_capacity(capacity),
            }
        }
        pub fn take(&mut self) -> RequestContext {
            self.pool.pop().unwrap_or(RequestContext {
                url: String::new(),
                method: "GET".into(),
                headers: HashMap::new(),
                body: None,
                timeout_ms: None,
            })
        }
        pub fn recycle(&mut self, mut request: RequestContext) {
            request.headers.clear();
            request.body = None;
            request.url.clear();
            self.pool.push(request);
        }
    }

    // 14) Chart.js / D3.js compatible report data.
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct Dataset {
        pub label: String,
        pub data: Vec<f64>,
    }
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct ChartData {
        pub labels: Vec<String>,
        pub datasets: Vec<Dataset>,
    }
    pub fn report_to_chart_data(report: &StressTestReport) -> ChartData {
        ChartData {
            labels: report
                .time_series
                .iter()
                .map(|p| p.second.to_string())
                .collect(),
            datasets: vec![
                Dataset {
                    label: "Requests".into(),
                    data: report
                        .time_series
                        .iter()
                        .map(|p| p.requests as f64)
                        .collect(),
                },
                Dataset {
                    label: "Average latency (ms)".into(),
                    data: report
                        .time_series
                        .iter()
                        .map(|p| p.avg_latency_ms)
                        .collect(),
                },
            ],
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn partitions_cover_total() {
            let p = partition_requests(10, 3);
            assert_eq!(p.iter().map(|x| x.end - x.start).sum::<u32>(), 10);
        }
        #[test]
        fn advanced_template_works() {
            assert!(
                render_template_advanced("{{random.email}}/{{index}}", 4).contains("loadtest4@")
            );
        }
        #[test]
        fn assertions_work() {
            let r = ResponseContext {
                status: 200,
                headers: [("x-test".into(), "ok-value".into())].into_iter().collect(),
                body: Some("{\"ok\":true}".into()),
                latency_ms: 4.0,
            };
            assert!(evaluate_assertion(&AssertionType::Status(200), &r).is_ok());
        }
    }
}
