//! Adaptive routing: pick the cheapest system that can answer a request.
//!
//! A request goes through stages, cheapest first, and stops at the first one that is sure:
//!
//! 1. **Rules** — known operations (the time, the date, arithmetic, finding a file) are recognised
//!    deterministically and sent to a tool. No model is involved.
//! 2. **Classifier** — a keyword scorer sorts the rest into chat, code or reasoning and asks for
//!    the model tier that fits. It costs microseconds and no memory.
//! 3. **Resolver** — when the classifier cannot tell, an optional [`Resolver`] (a micro-LLM in
//!    later versions) breaks the tie. Without one the request falls back to plain chat.
//!
//! The router only says what a request *deserves*. Whether the machine can afford that tier right
//! now is the resource governor's call, made by `lumbre-core`.

mod classify;
mod rules;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use classify::{classify, Scores};

/// Model sizes, smallest first. The names are Lumbre's profile families, not trained models.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Nano,
    Mini,
    Standard,
    Large,
    Code,
}

impl Tier {
    pub const ALL: [Tier; 5] = [Tier::Nano, Tier::Mini, Tier::Standard, Tier::Large, Tier::Code];

    /// Tiers to try, in order, when this one is not installed or does not fit.
    pub fn fallbacks(self) -> &'static [Tier] {
        match self {
            Tier::Code => &[Tier::Code, Tier::Standard, Tier::Mini, Tier::Nano],
            Tier::Large => &[Tier::Large, Tier::Standard, Tier::Mini, Tier::Nano],
            Tier::Standard => &[Tier::Standard, Tier::Mini, Tier::Nano],
            Tier::Mini => &[Tier::Mini, Tier::Nano],
            Tier::Nano => &[Tier::Nano],
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Nano => "nano",
            Tier::Mini => "mini",
            Tier::Standard => "standard",
            Tier::Large => "large",
            Tier::Code => "code",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    Chat,
    Code,
    Reasoning,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Route {
    /// Answer with a tool; no model needed.
    Tool { name: String, args: Value },
    /// Answer with a model of (at most) this tier.
    Model { tier: Tier, intent: Intent },
}

/// Which stage made the decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Rule,
    Classifier,
    Resolver,
    Fallback,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub route: Route,
    pub stage: Stage,
    /// 0..=1. Rules are 1.0; the classifier reports how lopsided its scores were.
    pub confidence: f32,
    /// One line for logs and for the "why this model?" affordance in the interface.
    pub explanation: String,
}

/// Breaks ties the classifier cannot. Implementations may be expensive (a small model).
pub trait Resolver: Send + Sync {
    fn resolve(&self, text: &str) -> Option<Intent>;
}

/// Names of the tools the rules can route to. `lumbre-tools` registers tools under these names.
pub mod tool {
    pub const CLOCK: &str = "clock.now";
    pub const CALC: &str = "calc.eval";
    pub const FIND_FILES: &str = "files.find";
}

#[derive(Default)]
pub struct Router {
    resolver: Option<Box<dyn Resolver>>,
}

impl std::fmt::Debug for Router {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Router").field("resolver", &self.resolver.is_some()).finish()
    }
}

impl Router {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_resolver(resolver: Box<dyn Resolver>) -> Self {
        Self { resolver: Some(resolver) }
    }

    pub fn route(&self, text: &str) -> Decision {
        let normalized = normalize(text);
        if let Some((route, explanation)) = rules::apply(text, &normalized) {
            return Decision { route, stage: Stage::Rule, confidence: 1.0, explanation };
        }
        let scores = classify(text, &normalized);
        match scores.verdict() {
            Some((intent, confidence)) => Decision {
                route: Route::Model { tier: tier_for(intent, &normalized), intent },
                stage: Stage::Classifier,
                confidence,
                explanation: format!(
                    "classified as {intent:?} (code {}, reasoning {})",
                    scores.code, scores.reasoning
                ),
            },
            None => match self.resolver.as_ref().and_then(|resolver| resolver.resolve(text)) {
                Some(intent) => Decision {
                    route: Route::Model { tier: tier_for(intent, &normalized), intent },
                    stage: Stage::Resolver,
                    confidence: 0.6,
                    explanation: format!("ambiguous; resolver chose {intent:?}"),
                },
                None => Decision {
                    route: Route::Model { tier: Tier::Standard, intent: Intent::Chat },
                    stage: Stage::Fallback,
                    confidence: 0.3,
                    explanation: format!(
                        "ambiguous (code {}, reasoning {}); no resolver, treated as chat",
                        scores.code, scores.reasoning
                    ),
                },
            },
        }
    }
}

fn tier_for(intent: Intent, normalized: &str) -> Tier {
    match intent {
        Intent::Code => Tier::Code,
        Intent::Reasoning => Tier::Large,
        // A greeting or a three-word question does not need the standard model.
        Intent::Chat if normalized.split_whitespace().count() <= 4 => Tier::Mini,
        Intent::Chat => Tier::Standard,
    }
}

/// Lowercase, without Spanish diacritics, question/exclamation marks or repeated spaces.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars().flat_map(char::to_lowercase) {
        let ch = match ch {
            'á' | 'à' => 'a',
            'é' | 'è' => 'e',
            'í' | 'ì' => 'i',
            'ó' | 'ò' => 'o',
            'ú' | 'ù' | 'ü' => 'u',
            'ñ' => 'n',
            '¿' | '?' | '¡' | '!' | '’' | '\'' => continue,
            other => other,
        };
        if ch.is_whitespace() {
            if !out.ends_with(' ') && !out.is_empty() {
                out.push(' ');
            }
        } else {
            out.push(ch);
        }
    }
    let trimmed = out.trim_end_matches([' ', '.', ',', ';', ':']).len();
    out.truncate(trimmed);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn route(text: &str) -> Decision {
        Router::new().route(text)
    }

    fn tool(decision: &Decision) -> (&str, &Value) {
        match &decision.route {
            Route::Tool { name, args } => (name.as_str(), args),
            other => panic!("expected a tool, got {other:?}"),
        }
    }

    #[test]
    fn normalize_folds_case_accents_and_marks() {
        assert_eq!(normalize("  ¿Qué   HORA es?  "), "que hora es");
        assert_eq!(normalize("What's the time?"), "whats the time");
    }

    #[test]
    fn the_time_and_the_date_never_reach_a_model() {
        for text in ["¿Qué hora es?", "what time is it", "Dime la hora", "What's the time?"] {
            let decision = route(text);
            assert_eq!(tool(&decision), (tool::CLOCK, &json!({"what": "time"})), "{text}");
            assert_eq!((decision.stage, decision.confidence), (Stage::Rule, 1.0));
        }
        for text in ["¿Qué día es hoy?", "what's the date today", "fecha de hoy", "¿A qué día estamos?"]
        {
            assert_eq!(tool(&route(text)), (tool::CLOCK, &json!({"what": "date"})), "{text}");
        }
    }

    #[test]
    fn a_long_question_that_mentions_the_time_is_not_hijacked() {
        let decision = route(
            "Explain why asking what time is it on a moving train depends on the observer in relativity",
        );
        assert!(matches!(decision.route, Route::Model { .. }), "{decision:?}");
    }

    #[test]
    fn arithmetic_goes_to_the_calculator() {
        let cases = [
            ("2+2", "2+2"),
            ("¿Cuánto es 12 * (3 + 4)?", "12 * (3 + 4)"),
            ("what is 2^10", "2^10"),
            ("calcula 7,5 / 3", "7,5 / 3"),
            ("3 x 4", "3 * 4"),
        ];
        for (text, expr) in cases {
            assert_eq!(tool(&route(text)), (tool::CALC, &json!({"expr": expr})), "{text}");
        }
        // A number alone, or prose with a number, is not arithmetic.
        assert!(matches!(route("42").route, Route::Model { .. }));
        assert!(matches!(route("what is 2 plus the meaning of life").route, Route::Model { .. }));
    }

    #[test]
    fn finding_a_document_goes_to_file_search() {
        let cases = [
            ("busca el archivo presupuesto 2026", "presupuesto 2026"),
            ("Encuentra el documento llamado Informe Final", "Informe Final"),
            ("find the file named notes.md", "notes.md"),
            ("¿Dónde está el fichero factura-marzo.pdf?", "factura-marzo.pdf"),
        ];
        for (text, query) in cases {
            assert_eq!(tool(&route(text)), (tool::FIND_FILES, &json!({"query": query})), "{text}");
        }
    }

    #[test]
    fn code_reasoning_and_chat_get_their_tiers() {
        let code = route("Fix this bug: fn main() { let x: u8 = 300; } does not compile");
        assert_eq!(code.route, Route::Model { tier: Tier::Code, intent: Intent::Code });
        assert_eq!(code.stage, Stage::Classifier);

        let reasoning = route("Explica paso a paso por qué la entropía de un sistema aislado no disminuye");
        assert_eq!(reasoning.route, Route::Model { tier: Tier::Large, intent: Intent::Reasoning });

        assert_eq!(route("hola").route, Route::Model { tier: Tier::Mini, intent: Intent::Chat });
        let chat = route("Recomiéndame tres novelas cortas para leer este verano en la playa");
        assert_eq!(chat.route, Route::Model { tier: Tier::Standard, intent: Intent::Chat });
    }

    struct AlwaysCode;
    impl Resolver for AlwaysCode {
        fn resolve(&self, _: &str) -> Option<Intent> {
            Some(Intent::Code)
        }
    }

    #[test]
    fn ambiguity_goes_to_the_resolver_or_falls_back_to_chat() {
        // One weak code signal and one weak reasoning signal: the classifier abstains.
        let text = "why is my code slow";
        let fallback = route(text);
        assert_eq!(fallback.stage, Stage::Fallback);
        assert_eq!(fallback.route, Route::Model { tier: Tier::Standard, intent: Intent::Chat });

        let resolved = Router::with_resolver(Box::new(AlwaysCode)).route(text);
        assert_eq!(resolved.stage, Stage::Resolver);
        assert_eq!(resolved.route, Route::Model { tier: Tier::Code, intent: Intent::Code });
    }

    #[test]
    fn fallbacks_always_end_at_the_smallest_tier_and_start_with_themselves() {
        for tier in Tier::ALL {
            assert_eq!(tier.fallbacks().first(), Some(&tier));
            assert_eq!(tier.fallbacks().last(), Some(&Tier::Nano));
        }
    }
}
