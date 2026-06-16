static ARTIFACT_0: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cosmic.postcard"));
static ARTIFACT_1: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cosmic-dark.postcard"));
static ARTIFACT_2: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cosmic-light.postcard"));
static ARTIFACT_3: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cupertino.postcard"));
static ARTIFACT_4: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cupertino-dark.postcard"));
static ARTIFACT_5: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cupertino-light.postcard"));
static ARTIFACT_6: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fluent.postcard"));
static ARTIFACT_7: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fluent-dark.postcard"));
static ARTIFACT_8: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fluent-light.postcard"));
static ARTIFACT_9: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/material.postcard"));
static ARTIFACT_10: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/material-dark.postcard"));
static ARTIFACT_11: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/material-light.postcard"));
static ARTIFACT_12: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/qt.postcard"));

pub(crate) fn generated_artifact(
    key: &super::FrozenBuiltinCacheKey,
) -> Option<&'static [u8]> {
    if key.resolved_style == "cosmic" && key.enable_experimental == false && key.debug_hooks == false && key.translation_domain.as_deref() == None && matches!(key.default_translation_context, super::FrozenDefaultTranslationContext::ComponentName) {
        return Some(ARTIFACT_0);
    }
    if key.resolved_style == "cosmic-dark" && key.enable_experimental == false && key.debug_hooks == false && key.translation_domain.as_deref() == None && matches!(key.default_translation_context, super::FrozenDefaultTranslationContext::ComponentName) {
        return Some(ARTIFACT_1);
    }
    if key.resolved_style == "cosmic-light" && key.enable_experimental == false && key.debug_hooks == false && key.translation_domain.as_deref() == None && matches!(key.default_translation_context, super::FrozenDefaultTranslationContext::ComponentName) {
        return Some(ARTIFACT_2);
    }
    if key.resolved_style == "cupertino" && key.enable_experimental == false && key.debug_hooks == false && key.translation_domain.as_deref() == None && matches!(key.default_translation_context, super::FrozenDefaultTranslationContext::ComponentName) {
        return Some(ARTIFACT_3);
    }
    if key.resolved_style == "cupertino-dark" && key.enable_experimental == false && key.debug_hooks == false && key.translation_domain.as_deref() == None && matches!(key.default_translation_context, super::FrozenDefaultTranslationContext::ComponentName) {
        return Some(ARTIFACT_4);
    }
    if key.resolved_style == "cupertino-light" && key.enable_experimental == false && key.debug_hooks == false && key.translation_domain.as_deref() == None && matches!(key.default_translation_context, super::FrozenDefaultTranslationContext::ComponentName) {
        return Some(ARTIFACT_5);
    }
    if key.resolved_style == "fluent" && key.enable_experimental == false && key.debug_hooks == false && key.translation_domain.as_deref() == None && matches!(key.default_translation_context, super::FrozenDefaultTranslationContext::ComponentName) {
        return Some(ARTIFACT_6);
    }
    if key.resolved_style == "fluent-dark" && key.enable_experimental == false && key.debug_hooks == false && key.translation_domain.as_deref() == None && matches!(key.default_translation_context, super::FrozenDefaultTranslationContext::ComponentName) {
        return Some(ARTIFACT_7);
    }
    if key.resolved_style == "fluent-light" && key.enable_experimental == false && key.debug_hooks == false && key.translation_domain.as_deref() == None && matches!(key.default_translation_context, super::FrozenDefaultTranslationContext::ComponentName) {
        return Some(ARTIFACT_8);
    }
    if key.resolved_style == "material" && key.enable_experimental == false && key.debug_hooks == false && key.translation_domain.as_deref() == None && matches!(key.default_translation_context, super::FrozenDefaultTranslationContext::ComponentName) {
        return Some(ARTIFACT_9);
    }
    if key.resolved_style == "material-dark" && key.enable_experimental == false && key.debug_hooks == false && key.translation_domain.as_deref() == None && matches!(key.default_translation_context, super::FrozenDefaultTranslationContext::ComponentName) {
        return Some(ARTIFACT_10);
    }
    if key.resolved_style == "material-light" && key.enable_experimental == false && key.debug_hooks == false && key.translation_domain.as_deref() == None && matches!(key.default_translation_context, super::FrozenDefaultTranslationContext::ComponentName) {
        return Some(ARTIFACT_11);
    }
    if key.resolved_style == "qt" && key.enable_experimental == false && key.debug_hooks == false && key.translation_domain.as_deref() == None && matches!(key.default_translation_context, super::FrozenDefaultTranslationContext::ComponentName) {
        return Some(ARTIFACT_12);
    }
    None
}

pub(crate) fn artifact_count() -> usize {
    13
}
