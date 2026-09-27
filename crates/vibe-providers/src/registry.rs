//! Named provider instances built from configuration.
//!
//! [`ProviderRegistry::from_config`] creates one provider per
//! `[providers.<name>]` entry according to its `kind`:
//!
//! | `kind` | Implementation |
//! |--------|----------------|
//! | `anthropic` | [`AnthropicProvider`] |
//! | `openai`, `ollama` | [`OpenAiCompatibleProvider`] |
//! | `mock` | [`MockProvider`] answering `extra.reply` (default `"Mock response."`) |
//!
//! Plugins contribute further kinds with
//! [`ProviderRegistryBuilder::register_kind`], or ready-made instances with
//! [`ProviderRegistryBuilder::provider`] / [`ProviderRegistry::register`].

use std::collections::BTreeMap;
use std::sync::Arc;

use vibe_core::provider::SharedProvider;
use vibe_core::{Error, ModelProvider, ModelRef, ProviderConfig, Result, VibeConfig};

use crate::anthropic::{AnthropicProvider, expand_model_shorthand};
use crate::mock::{MockProvider, text_response};
use crate::openai::OpenAiCompatibleProvider;

/// Builds a provider named `name` from its configuration.
pub type ProviderFactory =
    Arc<dyn Fn(&str, &ProviderConfig) -> Result<SharedProvider> + Send + Sync>;

#[derive(Clone)]
struct Entry {
    provider: SharedProvider,
    kind: String,
}

/// Providers by name, with model resolution.
#[derive(Clone, Default)]
pub struct ProviderRegistry {
    entries: BTreeMap<String, Entry>,
}

impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.entries.iter().map(|(n, e)| (n, &e.kind)))
            .finish()
    }
}

impl ProviderRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder able to learn extra provider kinds before reading the config.
    #[must_use]
    pub fn builder() -> ProviderRegistryBuilder {
        ProviderRegistryBuilder::new()
    }

    /// Build every configured provider with the built-in kinds.
    ///
    /// Missing API keys are not an error at this stage: the provider is
    /// created and reports [`vibe_core::ErrorKind::AuthFailed`] on first use.
    /// An unknown `kind` is a [`vibe_core::ErrorKind::Config`] error.
    pub fn from_config(config: &VibeConfig) -> Result<Self> {
        Self::builder().build(config)
    }

    /// Add or replace a provider under `name`.
    pub fn register(&mut self, name: impl Into<String>, provider: SharedProvider) -> &mut Self {
        self.register_with_kind(name, "custom", provider)
    }

    /// Add or replace a provider, recording its kind (which drives model
    /// shorthand expansion in [`Self::resolve`]).
    pub fn register_with_kind(
        &mut self,
        name: impl Into<String>,
        kind: impl Into<String>,
        provider: SharedProvider,
    ) -> &mut Self {
        self.entries.insert(
            name.into(),
            Entry {
                provider,
                kind: kind.into(),
            },
        );
        self
    }

    /// Provider registered under `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn ModelProvider>> {
        self.entries.get(name).map(|e| e.provider.clone())
    }

    /// Kind of the provider registered under `name`.
    #[must_use]
    pub fn kind(&self, name: &str) -> Option<&str> {
        self.entries.get(name).map(|e| e.kind.as_str())
    }

    /// Registered names, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    /// Number of providers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no provider is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Find the provider for `model` and the concrete model id to send.
    ///
    /// An empty model (or `default`) resolves to the provider's default model;
    /// on Anthropic providers shorthands such as `sonnet` are expanded.
    pub fn resolve(&self, model: &ModelRef) -> Result<(Arc<dyn ModelProvider>, String)> {
        let entry = self.entries.get(&model.provider).ok_or_else(|| {
            Error::config(format!(
                "unknown provider `{}` for model `{model}` (configured: {})",
                model.provider,
                if self.entries.is_empty() {
                    "none".to_string()
                } else {
                    self.names().join(", ")
                }
            ))
        })?;
        let requested = model.model.trim();
        let id = if requested.is_empty() || requested.eq_ignore_ascii_case("default") {
            entry.provider.info().default_model
        } else if entry.kind == "anthropic" {
            expand_model_shorthand(requested)
        } else {
            requested.to_string()
        };
        Ok((entry.provider.clone(), id))
    }

    /// Copy every provider into a core [`vibe_core::Registry`].
    pub fn install_into(&self, registry: &mut vibe_core::Registry) {
        for (name, entry) in &self.entries {
            registry.add_provider(name.clone(), entry.provider.clone());
        }
    }
}

/// Builder for [`ProviderRegistry`] with pluggable provider kinds.
#[derive(Clone)]
pub struct ProviderRegistryBuilder {
    factories: BTreeMap<String, ProviderFactory>,
    instances: Vec<(String, SharedProvider)>,
}

impl std::fmt::Debug for ProviderRegistryBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistryBuilder")
            .field("kinds", &self.factories.keys().collect::<Vec<_>>())
            .field(
                "instances",
                &self.instances.iter().map(|(n, _)| n).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Default for ProviderRegistryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderRegistryBuilder {
    /// Builder knowing the built-in kinds (`anthropic`, `openai`, `ollama`,
    /// `mock`).
    #[must_use]
    pub fn new() -> Self {
        let mut builder = Self {
            factories: BTreeMap::new(),
            instances: Vec::new(),
        };
        builder = builder
            .register_kind("anthropic", |name, cfg| {
                Ok(Arc::new(AnthropicProvider::from_config(name, cfg)?) as SharedProvider)
            })
            .register_kind("openai", |name, cfg| {
                Ok(Arc::new(OpenAiCompatibleProvider::from_config(name, cfg)?) as SharedProvider)
            })
            .register_kind("ollama", |name, cfg| {
                let mut cfg = cfg.clone();
                if cfg.base_url.is_none() {
                    cfg.base_url = Some(crate::openai::OLLAMA_BASE_URL.into());
                }
                Ok(Arc::new(OpenAiCompatibleProvider::from_config(name, &cfg)?) as SharedProvider)
            })
            .register_kind("mock", |name, cfg| {
                let reply = match cfg.extra.get("reply") {
                    Some(v) => v
                        .as_str()
                        .ok_or_else(|| {
                            Error::config(format!(
                                "provider `{name}`: `extra.reply` must be a string"
                            ))
                        })?
                        .to_string(),
                    None => "Mock response.".to_string(),
                };
                let mut mock = MockProvider::new()
                    .with_name(name)
                    .with_handler(move |_| Ok(text_response(reply.clone())));
                if let Some(model) = &cfg.default_model {
                    mock = mock.with_default_model(model);
                }
                Ok(Arc::new(mock) as SharedProvider)
            });
        builder
    }

    /// Teach the builder a new `kind` (or override a built-in one).
    #[must_use]
    pub fn register_kind<F>(mut self, kind: impl Into<String>, factory: F) -> Self
    where
        F: Fn(&str, &ProviderConfig) -> Result<SharedProvider> + Send + Sync + 'static,
    {
        self.factories.insert(kind.into(), Arc::new(factory));
        self
    }

    /// Add a ready-made provider. It overrides a configured one of the same
    /// name.
    #[must_use]
    pub fn provider(mut self, name: impl Into<String>, provider: SharedProvider) -> Self {
        self.instances.push((name.into(), provider));
        self
    }

    /// Kinds this builder can instantiate, sorted.
    #[must_use]
    pub fn kinds(&self) -> Vec<String> {
        self.factories.keys().cloned().collect()
    }

    /// Instantiate every provider of `config`, then add the ready-made ones.
    pub fn build(self, config: &VibeConfig) -> Result<ProviderRegistry> {
        let mut registry = ProviderRegistry::new();
        for (name, provider_cfg) in &config.providers {
            let kind = provider_cfg.kind.trim().to_ascii_lowercase();
            let factory = self.factories.get(&kind).ok_or_else(|| {
                Error::config(format!(
                    "provider `{name}` has unknown kind `{}` (known kinds: {})",
                    provider_cfg.kind,
                    self.kinds().join(", ")
                ))
            })?;
            let provider = factory(name, provider_cfg)?;
            registry.register_with_kind(name.clone(), kind, provider);
        }
        for (name, provider) in self.instances {
            registry.register(name, provider);
        }
        Ok(registry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibe_core::{CompletionRequest, ErrorKind, Message};

    #[test]
    fn default_config_builds_all_providers() {
        let reg = ProviderRegistry::from_config(&VibeConfig::default()).unwrap();
        assert_eq!(reg.names(), vec!["anthropic", "ollama", "openai"]);
        assert_eq!(reg.kind("ollama"), Some("openai"));
        assert!(!reg.is_empty());
        assert_eq!(reg.len(), 3);
    }

    #[test]
    fn resolve_expands_shorthands_and_defaults() {
        let reg = ProviderRegistry::from_config(&VibeConfig::default()).unwrap();
        let (p, id) = reg.resolve(&ModelRef::new("anthropic", "opus")).unwrap();
        assert_eq!(id, "claude-opus-5");
        assert_eq!(p.info().name, "anthropic");
        let (_, id) = reg.resolve(&ModelRef::new("anthropic", "")).unwrap();
        assert_eq!(id, "claude-sonnet-5");
        let (_, id) = reg
            .resolve(&ModelRef::parse("ollama/", "anthropic"))
            .unwrap();
        assert_eq!(id, "qwen2.5-coder");
        let (_, id) = reg.resolve(&ModelRef::new("ollama", "default")).unwrap();
        assert_eq!(id, "qwen2.5-coder");
        let (_, id) = reg.resolve(&ModelRef::new("openai", "opus")).unwrap();
        assert_eq!(id, "opus");
        let Err(err) = reg.resolve(&ModelRef::new("nope", "x")) else {
            panic!("unknown provider must fail");
        };
        assert_eq!(err.kind, ErrorKind::Config);
        assert!(err.message.contains("anthropic, ollama, openai"));
    }

    #[test]
    fn unknown_kind_is_config_error() {
        let cfg = VibeConfig::from_toml("[providers.x]\nkind = \"quantum\"\n").unwrap();
        let err = ProviderRegistry::from_config(&cfg).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Config);
        assert!(err.message.contains("quantum"));
    }

    #[tokio::test]
    async fn missing_key_surfaces_on_first_call() {
        let cfg = VibeConfig::from_toml(
            "[providers.a]\nkind = \"anthropic\"\napi_key_env = \"VIBE_PROVIDERS_TEST_SURELY_UNSET_1\"\n\
             [providers.o]\nkind = \"openai\"\napi_key_env = \"VIBE_PROVIDERS_TEST_SURELY_UNSET_2\"\n",
        )
        .unwrap();
        let reg = ProviderRegistry::from_config(&cfg).unwrap();
        for name in ["a", "o"] {
            let p = reg.get(name).unwrap();
            let err = p
                .complete(CompletionRequest::new("m", vec![Message::user("x")]))
                .await
                .unwrap_err();
            assert_eq!(err.kind, ErrorKind::AuthFailed, "provider {name}");
        }
    }

    #[tokio::test]
    async fn mock_kind_and_custom_kinds() {
        let cfg = VibeConfig::from_toml(
            "[providers.fake]\nkind = \"mock\"\n[providers.fake.extra]\nreply = \"hi\"\n\
             [providers.plug]\nkind = \"echo\"\n",
        )
        .unwrap();
        assert!(ProviderRegistry::from_config(&cfg).is_err());
        let reg = ProviderRegistry::builder()
            .register_kind("echo", |name, _| {
                Ok(Arc::new(MockProvider::new().with_name(name)) as SharedProvider)
            })
            .provider("extra", Arc::new(MockProvider::new()))
            .build(&cfg)
            .unwrap();
        assert_eq!(reg.names(), vec!["extra", "fake", "plug"]);
        let r = reg
            .get("fake")
            .unwrap()
            .complete(CompletionRequest::new("", vec![Message::user("x")]))
            .await
            .unwrap();
        assert_eq!(r.message.text(), "hi");
        assert_eq!(reg.get("plug").unwrap().info().name, "plug");
        assert_eq!(reg.kind("extra"), Some("custom"));

        let mut core = vibe_core::Registry::new();
        reg.install_into(&mut core);
        assert_eq!(core.providers.len(), 3);
    }

    #[test]
    fn register_overrides() {
        let mut reg = ProviderRegistry::new();
        reg.register("m", Arc::new(MockProvider::new().with_default_model("a")));
        reg.register("m", Arc::new(MockProvider::new().with_default_model("b")));
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.get("m").unwrap().info().default_model, "b");
        assert!(reg.get("zz").is_none());
    }
}
