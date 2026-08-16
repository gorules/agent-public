use crate::config::TsgoConfig;
use dashmap::DashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};
use tsgo_wasm::{ApiSession, TypeScript, TypeScriptConfig};

static TSGO_CWASM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tsgo.cwasm.zst"));

pub fn typescript(config: &TsgoConfig) -> anyhow::Result<TypeScript> {
    let engine = TypeScriptConfig {
        memory_limit: Some(config.memory_bytes),
        ..Default::default()
    };

    unsafe { engine.from_cwasm(TSGO_CWASM) }
}

const PROBE: &str = "__zen_probe.ts";

const TSCONFIG: &str = r#"{
  "compilerOptions": {
    "noEmit": true,
    "strict": true,
    "noImplicitAny": false,
    "noErrorTruncation": true,
    "target": "esnext",
    "module": "esnext",
    "moduleResolution": "bundler",
    "lib": ["esnext"],
    "skipLibCheck": true
  }
}"#;

/// Walks unions, objects and arrays so the probe prints a structural type
/// instead of whatever alias the source happened to name.
const TYPE_EXPANDER: &str = r#"type __grNul<T> = Extract<T, null | undefined>;
type __grVal<T> = Exclude<T, null | undefined>;
type __grExpand<T> = [__grVal<T>] extends [string] ? { [K in __grVal<T>]: `${K}` }[__grVal<T>] | __grNul<T>
  : [__grVal<T>] extends [number] ? { [K in __grVal<T>]: K }[__grVal<T>] | __grNul<T>
  : T extends Date ? Date
  : T extends Array<infer U> ? Array<__grExpand<U>>
  : T extends object ? (T extends infer O ? { [K in keyof O]: __grExpand<O[K]> } : never)
  : T;"#;

struct Probe {
    text: String,
    line: usize,
}

/// Assigning the handler's return type to a `never` binding makes tsc report
/// TS2322 with the fully expanded type as its message — that message is the
/// only channel tsgo offers for reading an inferred type back out.
fn build_probe(source: &str, input_ts: &str) -> Probe {
    let source = source.replace("\r\n", "\n");

    let mut text = format!("type FunctionInput = {input_ts};\n");
    text.push_str(&source);
    if !source.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("export {};\n");
    text.push_str(TYPE_EXPANDER);
    text.push('\n');
    text.push_str("declare const __fnResult: __grExpand<Awaited<ReturnType<typeof handler>>>;\n");

    let line = text.lines().count() + 1;
    text.push_str("const __zenExtract: never = __fnResult;\n");

    Probe { text, line }
}

fn extract_probe_type(message: &str) -> Option<String> {
    let rest = message.strip_prefix("Type '")?;
    let end = rest.find("' is not assignable to type 'never'")?;
    let extracted = &rest[..end];

    if extracted.is_empty() || extracted == "any" || extracted.contains("...") {
        return None;
    }

    Some(extracted.to_string())
}

fn cache_key(source: &str, input_ts: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    input_ts.hash(&mut hasher);
    hasher.finish()
}

pub struct TsgoAnalyzer {
    typescript: TypeScript,
    session: Mutex<Option<ApiSession>>,
    resolved: DashMap<u64, Option<Arc<str>>>,
    config: TsgoConfig,
}

impl TsgoAnalyzer {
    pub fn new(config: TsgoConfig) -> anyhow::Result<Self> {
        Ok(Self {
            typescript: typescript(&config)?,
            session: Mutex::new(None),
            resolved: DashMap::new(),
            config,
        })
    }

    /// Resolves the TypeScript return type of `handler` in `source` when
    /// called with `input_ts`. `None` means the type is unusable (`any`,
    /// truncated) or tsgo could not produce one — both are cached, so a
    /// pathological function is probed once per process, not once per rule.
    pub fn resolve(&self, source: &str, input_ts: &str) -> Option<Arc<str>> {
        let key = cache_key(source, input_ts);
        if let Some(hit) = self.resolved.get(&key) {
            return hit.clone();
        }

        let resolved = match self.probe(source, input_ts) {
            Ok(resolved) => resolved,
            Err(error) => {
                tracing::warn!(error = %error, "tsgo probe failed; function type stays unresolved");
                None
            }
        };

        if self.resolved.len() >= self.config.cache_capacity {
            self.resolved.clear();
        }
        self.resolved.insert(key, resolved.clone());
        resolved
    }

    fn probe(&self, source: &str, input_ts: &str) -> anyhow::Result<Option<Arc<str>>> {
        let probe = build_probe(source, input_ts);

        let diagnostics = self.with_session(|session| {
            session.update_file(PROBE, &probe.text)?;
            session.diagnostics_for(PROBE)
        })?;

        let extracted = diagnostics.iter().find_map(|diagnostic| {
            let on_probe_line = diagnostic
                .range
                .is_some_and(|range| range.start.line as usize == probe.line);

            (diagnostic.code == 2322 && on_probe_line)
                .then(|| extract_probe_type(&diagnostic.text))
                .flatten()
        });

        if extracted.is_none()
            && let Some(first) = diagnostics.iter().find(|d| d.is_error())
        {
            tracing::debug!(code = first.code, message = %first.text, "tsgo function diagnostic");
        }

        Ok(extracted.map(Arc::from))
    }

    /// A trapped or timed-out guest leaves the session unusable, so a failed
    /// call drops it and the next one boots a fresh guest rather than
    /// returning the same error forever.
    fn with_session<T>(
        &self,
        run: impl FnOnce(&mut ApiSession) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let mut slot = self
            .session
            .lock()
            .map_err(|_| anyhow::anyhow!("tsgo session mutex poisoned"))?;

        if slot.is_none() {
            *slot = Some(self.typescript.api_session(
                &[("tsconfig.json", TSCONFIG), (PROBE, "export {};\n")],
                self.config.timeout,
            )?);
        }

        let session = slot.as_mut().expect("session created above");
        match run(session) {
            Ok(value) => Ok(value),
            Err(error) => {
                *slot = None;
                Err(error)
            }
        }
    }
}

static ANALYZER: OnceLock<Option<Arc<TsgoAnalyzer>>> = OnceLock::new();

/// Installs the process-wide analyzer. Deserializing the precompiled module
/// costs ~1s, so this runs at startup rather than on the first request.
pub fn init(config: &TsgoConfig) {
    ANALYZER.get_or_init(|| {
        if !config.enabled {
            tracing::info!("tsgo disabled; function node types stay unresolved");
            return None;
        }

        match TsgoAnalyzer::new(config.clone()) {
            Ok(analyzer) => {
                tracing::info!("Loaded precompiled tsgo module");
                Some(Arc::new(analyzer))
            }
            Err(error) => {
                tracing::error!(error = ?error, "Failed to load tsgo; schemas fall back to declared");
                None
            }
        }
    });
}

pub fn analyzer() -> Option<Arc<TsgoAnalyzer>> {
    ANALYZER.get().cloned().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyzer() -> TsgoAnalyzer {
        TsgoAnalyzer::new(TsgoConfig::default()).unwrap()
    }

    #[test]
    fn precompiled_module_loads() {
        let ts = typescript(&TsgoConfig::default()).unwrap();
        assert!(ts.version().unwrap().starts_with("Version"));
    }

    #[test]
    fn probe_line_points_at_the_extraction_assignment() {
        let probe = build_probe(
            "export const handler = async (input: FunctionInput) => input;",
            "{ a: number }",
        );

        let lines: Vec<&str> = probe.text.lines().collect();
        assert_eq!(
            lines[probe.line - 1],
            "const __zenExtract: never = __fnResult;"
        );
    }

    #[test]
    fn probe_types_are_extracted_and_rejected_when_useless() {
        assert_eq!(
            extract_probe_type("Type '{ a: number; }' is not assignable to type 'never'."),
            Some("{ a: number; }".to_string())
        );
        assert_eq!(
            extract_probe_type("Type 'any' is not assignable to type 'never'."),
            None
        );
        assert_eq!(extract_probe_type("something else"), None);
    }

    #[test]
    fn resolves_a_return_type() {
        let resolved = analyzer().resolve(
            "export const handler = (input: FunctionInput) => ({ doubled: input.a * 2 });",
            "{ a: number }",
        );

        assert_eq!(resolved.as_deref(), Some("{ doubled: number; }"));
    }

    #[test]
    fn resolves_through_an_awaited_promise() {
        let resolved = analyzer().resolve(
            "export const handler = async (input: FunctionInput) => ({ name: input.name, ok: true });",
            "{ name: string }",
        );

        assert_eq!(resolved.as_deref(), Some("{ name: string; ok: boolean; }"));
    }

    #[test]
    fn unresolvable_functions_are_cached_as_none() {
        let analyzer = analyzer();
        let source = "export const handler = (input: FunctionInput): any => input;";

        assert_eq!(analyzer.resolve(source, "{ a: number }"), None);
        assert_eq!(analyzer.resolve(source, "{ a: number }"), None);
        assert_eq!(analyzer.resolved.len(), 1);
    }

    #[test]
    fn repeated_sources_hit_the_cache() {
        let analyzer = analyzer();
        let source = "export const handler = (input: FunctionInput) => ({ v: input.a });";

        let first = analyzer.resolve(source, "{ a: string }");
        let second = analyzer.resolve(source, "{ a: string }");

        assert_eq!(first, second);
        assert_eq!(
            analyzer.resolved.len(),
            1,
            "one entry for one (source, input)"
        );

        analyzer.resolve(source, "{ a: number }");
        assert_eq!(analyzer.resolved.len(), 2, "input type is part of the key");
    }

    /// A guest that traps must not wedge the analyzer: the session is dropped
    /// and rebuilt, so later probes still resolve.
    #[test]
    fn survives_a_pathological_source() {
        let analyzer = analyzer();

        analyzer.resolve("this is not typescript at all ((((", "{ a: number }");

        let resolved = analyzer.resolve(
            "export const handler = (input: FunctionInput) => ({ ok: input.a });",
            "{ a: number }",
        );
        assert_eq!(resolved.as_deref(), Some("{ ok: number; }"));
    }

    /// The containment promise: a guest that cannot even start — here because
    /// its memory ceiling is far below what the compiler needs — surfaces as
    /// an unresolved type, not as a panic out of `resolve`.
    #[test]
    fn a_guest_that_cannot_run_resolves_to_none() {
        let analyzer = TsgoAnalyzer::new(TsgoConfig {
            memory_bytes: 1 << 16,
            ..Default::default()
        })
        .expect("loading the module is independent of the store limit");

        let resolved = analyzer.resolve(
            "export const handler = (input: FunctionInput) => ({ ok: input.a });",
            "{ a: number }",
        );

        assert_eq!(resolved, None);
    }
}
