import { invoke } from '@tauri-apps/api/core'

import { isTauri } from '#lib/auth.js'
import { locales, overwriteGetLocale, setLocale } from '#lib/paraglide/runtime.js'
import type { Locale } from '#lib/paraglide/runtime.js'

export type { Locale }

export const SUPPORTED_LOCALES = locales

const LOCALE_STORAGE_KEY = 'stravia-locale'

// 桌面托盘菜单等原生文案跟随界面语言；同步是尽力而为，失败不影响界面。
function syncDesktopLocale(locale: Locale): void {
  if (!isTauri) return
  void invoke('set_desktop_locale', { locale }).catch(() => console.warn('Stravia desktop locale sync failed'))
}

function isLocale(value: string): value is Locale {
  return SUPPORTED_LOCALES.some((locale) => locale === value)
}

class LocaleState {
  current = $state<Locale>('en-US')
  #desktopSyncReady = false

  restore(): void {
    // app.html 已在解析 head 时按保存值和浏览器偏好选择语言。
    const initial = document.documentElement.lang
    this.set(isLocale(initial) ? initial : 'en-US')
  }

  set(next: Locale, persist = true): void {
    void setLocale(next, { reload: false })
    this.current = next
    document.documentElement.lang = next
    if (persist) localStorage.setItem(LOCALE_STORAGE_KEY, next)
    if (this.#desktopSyncReady) syncDesktopLocale(next)
  }

  enableDesktopSync(): void {
    if (!isTauri || this.#desktopSyncReady) return
    this.#desktopSyncReady = true
    syncDesktopLocale(this.current)
  }
}

export const localeState = new LocaleState()
overwriteGetLocale(() => localeState.current)
