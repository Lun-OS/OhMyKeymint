import './webview.scss'

// Matches the vite build target (chrome94). The bundle contains no runtime
// API newer than Array.prototype.at (Chrome 92), and color-mix()/dvh CSS
// declarations always ship with fallback declarations, so 94 is the real
// floor rather than a conservative guess.
const MIN_ANDROID_WEBVIEW_VERSION = 94

interface NavigatorUAData {
  readonly brands: ReadonlyArray<{ readonly brand: string; readonly version: string }>
}

export function getWebviewVersion(): number | null {
  const brands = (navigator as Navigator & { readonly userAgentData?: NavigatorUAData }).userAgentData?.brands
  if (Array.isArray(brands) && brands.length > 0) {
    const webViewBrand = brands.find(entry => entry.brand === 'Android WebView')
    if (webViewBrand) return Number.parseInt(webViewBrand.version, 10)
  }

  const userAgent = navigator.userAgent
  if (/Android/i.test(userAgent) && /\bwv\b/.test(userAgent)) {
    const match = userAgent.match(/Chrome\/(\d+)/)
    return match ? Number.parseInt(match[1], 10) : 0
  }
  return null
}

export function isSupported(): boolean {
  const version = getWebviewVersion()
  return version === null || version >= MIN_ANDROID_WEBVIEW_VERSION
}

export function renderBlockingPage(): HTMLElement {
  const container = document.createElement('div')
  container.className = 'webview'
  const message = document.createElement('p')
  message.textContent = 'Android System WebView 94 or newer is required. Update "Android System WebView" (or Chrome) in Google Play, then reopen this page.'
  container.appendChild(message)
  return container
}
