//! Explicit language selection. Provider support is checked before capture.
use crate::vocabulary::ProfileFile as _;
pub const DEEPGRAM_LANGUAGES: &str = "af af-ZA ar ar-AE ar-SA ar-QA ar-KW ar-SY ar-LB ar-PS ar-JO ar-EG ar-SD ar-TD ar-MA ar-DZ ar-TN ar-IQ ar-IR hy as as-IN be bn bs bg ca zh-HK zh zh-CN zh-Hans zh-TW zh-Hant hr cs cs-CZ da da-DK nl en en-US en-AU en-GB en-IN en-NZ et fi nl-BE fr fr-CA ka ka-GE de de-CH el gu gu-IN he hi hu id it ja kn kk kk-KZ ko ko-KR lv lt mk ms mr mn ne no ps ps-AF fa pl pt pt-BR pt-PT pa pa-IN ro ru sr sk sl es es-419 sv sv-SE tl ta te th th-TH tr tr-TR uk ur vi";
/// Accept a bounded language tag, not provider switches or query fragments.
/// Detection/code-switching are separate capabilities and are not implied here.
pub fn validate_tag(tag: &str) -> Result<(), String> {
    fotw_stt::vocabulary::validate_language(tag)
}

/// Nova-3 monolingual codes verified against the official model/language table
/// on 2026-10-24. Do not silently change model or substitute multilingual hints.
/// https://developers.deepgram.com/docs/models-languages-overview#nova-3
pub fn validate_deepgram(tag: &str) -> Result<(), String> {
    validate_tag(tag)?;

    if !DEEPGRAM_LANGUAGES
        .split_whitespace()
        .any(|code| code == tag)
    {
        return Err("Selected language is not supported by Murmur's Nova-3 adapter; choose a supported language or local provider".into());
    }
    Ok(())
}

/// Save a language choice without replacing independently edited vocabulary.
pub fn save_choice(path: &std::path::Path, code: &str) -> Result<(), String> {
    validate_tag(code)?;
    crate::vocabulary::Profile::update(path, |profile| {
        profile.language = code.to_owned();
        Ok(())
    })
}
