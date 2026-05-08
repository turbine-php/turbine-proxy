import { i18n } from '@lingui/core'

// ── Locale detection & persistence ──────────────────────────────────────────

const STORAGE_KEY = 'turbineproxy-locale'
export const SUPPORTED = ['en', 'pt', 'fr', 'es', 'de', 'pl', 'zh']
export const LOCALE_LABELS = {
  en: 'English',
  pt: 'Português',
  fr: 'Français',
  es: 'Español',
  de: 'Deutsch',
  pl: 'Polski',
  zh: '中文',
}

function detectLocale() {
  const stored = localStorage.getItem(STORAGE_KEY)
  if (stored && SUPPORTED.includes(stored)) return stored
  const browser = navigator.language?.split('-')[0]
  return SUPPORTED.includes(browser) ? browser : 'en'
}

export function saveLocale(locale) {
  localStorage.setItem(STORAGE_KEY, locale)
}

// ── Dynamic catalog loading ──────────────────────────────────────────────────
// Catalogs are loaded lazily so the initial bundle only includes the active locale.

const loadedLocales = new Set()

export async function activateLocale(locale) {
  if (!loadedLocales.has(locale)) {
    const { messages } = await import(`./locales/${locale}.js`)
    i18n.load(locale, messages)
    loadedLocales.add(locale)
  }
  i18n.activate(locale)
  saveLocale(locale)
}

// ── Bootstrap ────────────────────────────────────────────────────────────────

const initial = detectLocale()

// Eagerly load the detected locale (resolved before React renders)
export async function initI18n() {
  const { messages } = await import(`./locales/${initial}.js`)
  i18n.load(initial, messages)
  i18n.activate(initial)
  loadedLocales.add(initial)
}

export { i18n }
