/* tslint:disable */
/* eslint-disable */

export class DistributedTest {
    free(): void;
    [Symbol.dispose](): void;
    constructor(worker_count: number);
    partitions(total: number): any;
}

/**
 * কাস্টম অ্যাসারশন চেকার
 */
export function evaluate_custom_assertion(status: number, body: string, expected_status: number, expected_text: string): boolean;

export function metrics_snapshot_json(snapshot: any): string;

/**
 * Web Worker মাল্টি-থ্রেডিং পার্টিশনার
 */
export function partition_requests_for_workers(total_requests: number, workers: number): any;

export function register_interceptor(name: string, js_fn: Function): void;

/**
 * ডাইনামিক টেমপ্লেট ও ফাকার টেস্ট প্রিভিউ
 */
export function render_template_preview(template: string, index: number): string;

export function run_stress_test(url: string, total_requests: number, concurrency: number, method: string, headers_json: string, body: string | null | undefined, timeout_ms: number | null | undefined, follow_redirects: boolean, load_pattern: string, expected_status: number, expected_text: string, custom_script: string, progress_callback: Function, rate_limit?: number | null, circuit_threshold?: number | null, retry_attempts?: number | null, use_user_agent_rotation?: boolean | null, use_waf_bypass?: boolean | null, waf_cache_buster?: boolean | null, waf_xff_rotation?: boolean | null, waf_random_headers?: boolean | null, rate_limit_max_wait_ms?: number | null, retry_base_delay_ms?: number | null, retry_max_delay_ms?: number | null, circuit_timeout_ms?: bigint | null, circuit_window_size?: number | null, circuit_window_ms?: bigint | null, reset_window_on_probe_success?: boolean | null, enable_templating?: boolean | null, think_time_ms?: number | null): Promise<any>;

export function validate_config(total_requests: number, concurrency: number): any;

export function version(): string;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_distributedtest_free: (a: number, b: number) => void;
    readonly distributedtest_new: (a: number) => number;
    readonly distributedtest_partitions: (a: number, b: number) => any;
    readonly evaluate_custom_assertion: (a: number, b: number, c: number, d: number, e: number, f: number) => number;
    readonly metrics_snapshot_json: (a: any) => [number, number, number, number];
    readonly partition_requests_for_workers: (a: number, b: number) => [number, number, number];
    readonly register_interceptor: (a: number, b: number, c: any) => void;
    readonly render_template_preview: (a: number, b: number, c: number) => [number, number];
    readonly run_stress_test: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number, m: number, n: number, o: number, p: number, q: number, r: number, s: number, t: any, u: number, v: number, w: number, x: number, y: number, z: number, a1: number, b1: number, c1: number, d1: number, e1: number, f1: number, g1: bigint, h1: number, i1: number, j1: bigint, k1: number, l1: number, m1: number) => any;
    readonly validate_config: (a: number, b: number) => [number, number, number];
    readonly version: () => [number, number];
    readonly wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___js_sys_20834107324bc873___Function_fn_wasm_bindgen_2a67c6f173b08fad___JsValue_____wasm_bindgen_2a67c6f173b08fad___sys__Undefined___js_sys_20834107324bc873___Function_fn_wasm_bindgen_2a67c6f173b08fad___JsValue_____wasm_bindgen_2a67c6f173b08fad___sys__Undefined_______true_: (a: number, b: number, c: any, d: any) => void;
    readonly wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___wasm_bindgen_2a67c6f173b08fad___JsValue__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_2a67c6f173b08fad___JsError___true_: (a: number, b: number, c: any) => [number, number];
    readonly wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke_______true_: (a: number, b: number) => void;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __wbindgen_destroy_closure: (a: number, b: number) => void;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
