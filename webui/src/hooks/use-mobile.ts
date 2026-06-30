import * as React from "react"

const MOBILE_BREAKPOINT = 768

function getMobileSnapshot() {
  return window.innerWidth < MOBILE_BREAKPOINT
}

function getServerMobileSnapshot() {
  return false
}

function subscribeMobile(callback: () => void) {
  if (!window.matchMedia) {
    window.addEventListener("resize", callback)
    return () => window.removeEventListener("resize", callback)
  }

  const mql = window.matchMedia(`(max-width: ${MOBILE_BREAKPOINT - 1}px)`)
  mql.addEventListener("change", callback)
  return () => mql.removeEventListener("change", callback)
}

export function useIsMobile() {
  return React.useSyncExternalStore(
    subscribeMobile,
    getMobileSnapshot,
    getServerMobileSnapshot
  )
}
