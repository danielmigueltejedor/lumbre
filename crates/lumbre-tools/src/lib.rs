//! Tools: things Lumbre can do without a language model.
//!
//! Every tool declares the [`Permission`] it needs. A [`Registry`] refuses to run a tool whose
//! permission the caller has not granted, so "what may this component touch" is one explicit set,
//! not something scattered through the code.

mod calc;
mod clock;
mod files;

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use calc::{evaluate, format_number, CalcError};

/// What a tool needs in order to run. Ordered roughly by how much damage it could do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// Pure computation or reading the clock.
    None,
    /// Read file names and contents under the configured roots.
    ReadFiles,
    /// Make network requests.
    Network,
    /// Create or modify files.
    WriteFiles,
    /// Run programs.
    Execute,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    #[default]
    En,
    Es,
}

impl Language {
    /// Spanish if the text looks Spanish, English otherwise. Good enough to phrase a tool answer.
    pub fn detect(text: &str) -> Self {
        let lower = text.to_lowercase();
        let spanish = lower.contains(['¿', '¡', 'ñ', 'á', 'é', 'í', 'ó', 'ú'])
            || lower.split_whitespace().any(|word| {
                matches!(
                    word,
                    "que" | "el" | "la" | "es" | "hora" | "dia" | "cuanto" | "busca" | "archivo" | "hoy"
                )
            });
        if spanish {
            Language::Es
        } else {
            Language::En
        }
    }
}

/// What the caller allows and where.
#[derive(Clone, Debug, Default)]
pub struct Context {
    pub granted: Vec<Permission>,
    /// Directories file tools may look into. Nothing outside them is ever read.
    pub roots: Vec<PathBuf>,
    pub language: Language,
}

impl Context {
    pub fn allows(&self, permission: Permission) -> bool {
        permission == Permission::None || self.granted.contains(&permission)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Output {
    /// A sentence for the user.
    pub text: String,
    /// The same answer as data, for interfaces and for models that call tools.
    pub data: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("no tool named `{0}`")]
    Unknown(String),
    #[error("`{tool}` needs the `{needs:?}` permission, which has not been granted")]
    Denied { tool: String, needs: Permission },
    #[error("invalid arguments: {0}")]
    Arguments(String),
    #[error("{0}")]
    Failed(String),
}

pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn permission(&self) -> Permission;
    fn invoke(&self, args: &Value, context: &Context) -> Result<Output, ToolError>;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub permission: Permission,
}

#[derive(Default)]
pub struct Registry {
    tools: BTreeMap<&'static str, Box<dyn Tool>>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.tools.keys()).finish()
    }
}

impl Registry {
    /// The tools that ship with Lumbre 0.1.
    pub fn builtin() -> Self {
        let mut registry = Self::default();
        registry.register(Box::new(clock::Clock));
        registry.register(Box::new(calc::Calc));
        registry.register(Box::new(files::FindFiles));
        registry
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name(), tool);
    }

    pub fn list(&self) -> Vec<ToolInfo> {
        self.tools
            .values()
            .map(|tool| ToolInfo {
                name: tool.name().to_owned(),
                description: tool.description().to_owned(),
                permission: tool.permission(),
            })
            .collect()
    }

    pub fn invoke(&self, name: &str, args: &Value, context: &Context) -> Result<Output, ToolError> {
        let tool = self.tools.get(name).ok_or_else(|| ToolError::Unknown(name.to_owned()))?;
        if !context.allows(tool.permission()) {
            return Err(ToolError::Denied { tool: name.to_owned(), needs: tool.permission() });
        }
        tool.invoke(args, context)
    }
}

pub(crate) fn string_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::Arguments(format!("missing string `{key}`")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builtin_tools_are_listed_with_their_permissions() {
        let list = Registry::builtin().list();
        let summary: Vec<_> = list.iter().map(|tool| (tool.name.as_str(), tool.permission)).collect();
        assert_eq!(
            summary,
            [
                ("calc.eval", Permission::None),
                ("clock.now", Permission::None),
                ("files.find", Permission::ReadFiles)
            ]
        );
    }

    #[test]
    fn a_tool_without_its_permission_does_not_run() {
        let registry = Registry::builtin();
        let error = registry.invoke("files.find", &json!({"query": "x"}), &Context::default()).unwrap_err();
        assert!(matches!(error, ToolError::Denied { needs: Permission::ReadFiles, .. }), "{error}");
        assert!(matches!(
            registry.invoke("nope", &json!({}), &Context::default()),
            Err(ToolError::Unknown(_))
        ));
    }

    #[test]
    fn language_detection() {
        assert_eq!(Language::detect("¿Qué hora es?"), Language::Es);
        assert_eq!(Language::detect("que hora es"), Language::Es);
        assert_eq!(Language::detect("what time is it"), Language::En);
    }
}
