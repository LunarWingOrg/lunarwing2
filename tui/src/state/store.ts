/**
 * The external store (design 3.3, area-client 2.1). `dispatch` runs the pure reducer and notifies subscribers
 * when the state object changed; React reads it with useSyncExternalStore(store.subscribe, store.getState),
 * which replaces vibetui4's ref/state mirroring (v4:tui/src/app.tsx:39-69).
 *
 * Mount once: createElement(StoreProvider, { store }, <App />) (or <StoreContext.Provider value={store}>).
 */
import { createContext, createElement, useContext, useSyncExternalStore, type ReactNode } from "react"
import type { Action } from "./actions.ts"
import type { AppState, Store } from "./model.ts"
import { rootReducer, type Reducer } from "./reducer.ts"

export type { Store } from "./model.ts"

export function createStore(initial: AppState, reduce: Reducer = rootReducer): Store {
  let state = initial
  const listeners = new Set<() => void>()
  let dispatching = false

  const notify = (): void => {
    let failure: { error: unknown } | null = null
    for (const listener of [...listeners]) {
      try {
        listener()
      } catch (error) {
        failure ??= { error }
      }
    }
    if (failure !== null) throw failure.error
  }

  const run = (actions: readonly Action[]): void => {
    if (dispatching) throw new Error("store: dispatch from inside the reducer")
    const before = state
    dispatching = true
    try {
      for (const action of actions) state = reduce(state, action)
    } finally {
      dispatching = false
    }
    if (state !== before) notify()
  }

  return {
    getState: () => state,
    dispatch: (action) => run([action]),
    dispatchMany: (actions) => {
      if (actions.length > 0) run(actions)
    },
    subscribe: (listener) => {
      listeners.add(listener)
      return () => {
        listeners.delete(listener)
      }
    },
  }
}

export const StoreContext = createContext<Store | null>(null)

export function StoreProvider(props: { store: Store; children?: ReactNode }): ReactNode {
  return createElement(StoreContext.Provider, { value: props.store }, props.children)
}

export function useStore(): Store {
  const store = useContext(StoreContext)
  if (store === null) throw new Error("useStore: no StoreProvider above this component")
  return store
}

/** Whole-state subscription. */
export function useAppState(): AppState {
  const store = useStore()
  return useSyncExternalStore(store.subscribe, store.getState, store.getState)
}

/**
 * A slice subscription: re-renders only when `select` returns a different value (Object.is). `select` must
 * return existing objects from the state (or primitives), never a new object per call.
 */
export function useAppSelector<T>(select: (state: AppState) => T): T {
  const store = useStore()
  const read = (): T => select(store.getState())
  return useSyncExternalStore(store.subscribe, read, read)
}
