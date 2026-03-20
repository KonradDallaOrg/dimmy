/**
 * Minimal i18n module for Dimmy.
 *
 * Usage:
 *   import { t, setLocale, applyTranslations } from './i18n.js';
 *   await setLocale('it');          // load Italian
 *   applyTranslations();            // translate all data-i18n elements
 *   const s = t('onboarding.welcome'); // lookup a key
 *
 * Adding a language = adding one JSON file in locales/.
 * Zero code changes needed.
 */

const FALLBACK_LOCALE = 'en';
let currentLocale = FALLBACK_LOCALE;
let strings = {};       // current locale strings (flat: "section.key" → "value")
let fallbackStrings = {}; // English fallback

/**
 * Load a locale JSON file. Returns flat key→value map.
 */
async function loadLocaleFile(locale) {
  try {
    const resp = await fetch(`locales/${locale}.json`);
    if (!resp.ok) return null;
    return await resp.json();
  } catch (_) {
    return null;
  }
}

/**
 * Set the active locale. Loads the JSON file and applies translations.
 * Falls back to English if the locale file doesn't exist.
 */
export async function setLocale(locale) {
  // Always load English as fallback
  if (!fallbackStrings || Object.keys(fallbackStrings).length === 0) {
    fallbackStrings = (await loadLocaleFile(FALLBACK_LOCALE)) || {};
  }

  if (locale === FALLBACK_LOCALE) {
    strings = fallbackStrings;
  } else {
    const loaded = await loadLocaleFile(locale);
    strings = loaded || fallbackStrings;
  }

  currentLocale = locale;
  applyTranslations();
}

/**
 * Get a translated string by key. Returns the key itself if not found.
 * Supports simple {{placeholder}} interpolation.
 *
 *   t('status.processing', { current: 2, total: 5 })
 *   → "Processing 2/5..."
 */
export function t(key, params) {
  let val = strings[key] ?? fallbackStrings[key] ?? key;
  if (params) {
    for (const [k, v] of Object.entries(params)) {
      val = val.replace(new RegExp(`\\{\\{${k}\\}\\}`, 'g'), String(v));
    }
  }
  return val;
}

/**
 * Get current locale code.
 */
export function getLocale() {
  return currentLocale;
}

/**
 * Apply translations to all elements with data-i18n attribute.
 *
 * Supports:
 *   data-i18n="key"                    → sets textContent
 *   data-i18n-placeholder="key"        → sets placeholder
 *   data-i18n-title="key"              → sets title
 *   data-i18n-html="key"               → sets innerHTML (use sparingly)
 */
export function applyTranslations() {
  document.querySelectorAll('[data-i18n]').forEach(el => {
    const key = el.getAttribute('data-i18n');
    const val = t(key);
    if (val !== key) el.textContent = val;
  });

  document.querySelectorAll('[data-i18n-placeholder]').forEach(el => {
    const key = el.getAttribute('data-i18n-placeholder');
    const val = t(key);
    if (val !== key) el.placeholder = val;
  });

  document.querySelectorAll('[data-i18n-title]').forEach(el => {
    const key = el.getAttribute('data-i18n-title');
    const val = t(key);
    if (val !== key) el.title = val;
  });

  document.querySelectorAll('[data-i18n-html]').forEach(el => {
    const key = el.getAttribute('data-i18n-html');
    const val = t(key);
    if (val !== key) el.innerHTML = val;
  });
}

/**
 * Detect preferred UI locale from config language or system.
 * Maps STT language codes (it, en, de...) to UI locale codes.
 * Returns 'en' if no match.
 */
export function detectLocale(configLanguage) {
  // Direct match (config language = "it", "en", "de", etc.)
  if (configLanguage && configLanguage.length >= 2) {
    const code = configLanguage.substring(0, 2).toLowerCase();
    return code;
  }
  // System locale fallback
  const nav = navigator.language || navigator.userLanguage || 'en';
  return nav.substring(0, 2).toLowerCase();
}
