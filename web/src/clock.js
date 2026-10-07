// Time source shared by every animation. In capture mode time is virtual:
// it only advances when the recorder calls tick(), so recordings are
// deterministic regardless of how slowly a frame renders.

export class Clock {
  constructor(virtual) {
    this.virtual = virtual
    this.t = 0
    this.timers = []
    this.pending = 0
  }

  now() {
    return this.virtual ? this.t : performance.now()
  }

  sleep(ms) {
    if (!this.virtual) return new Promise((r) => setTimeout(r, ms))
    return new Promise((resolve) => this.timers.push({ at: this.t + ms, resolve }))
  }

  advance(ms) {
    this.t += ms
    const due = this.timers.filter((x) => x.at <= this.t)
    this.timers = this.timers.filter((x) => x.at > this.t)
    for (const x of due) x.resolve()
  }

  /** Track real I/O so a virtual tick waits for it to finish. */
  track(promise) {
    this.pending++
    return promise.finally(() => this.pending--)
  }

  async settle() {
    const macrotask = () => new Promise((r) => setTimeout(r, 0))
    for (let i = 0; i < 4; i++) {
      await macrotask()
      while (this.pending > 0) await macrotask()
    }
  }
}

export const ease = {
  linear: (t) => t,
  inOut: (t) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2),
  out: (t) => 1 - Math.pow(1 - t, 3),
  back: (t) => {
    const c1 = 1.5
    const c3 = c1 + 1
    return 1 + c3 * Math.pow(t - 1, 3) + c1 * Math.pow(t - 1, 2)
  },
}

/** Keyed tweens evaluated once per frame against the clock. */
export class Animator {
  constructor(clock) {
    this.clock = clock
    this.items = new Map()
  }

  add(key, duration, update, easing = ease.inOut) {
    const prev = this.items.get(key)
    if (prev) prev.finish(false)
    return new Promise((resolve) => {
      const item = {
        start: this.clock.now(),
        duration: Math.max(1, duration),
        update,
        easing,
        finish: (run = true) => {
          if (run) update(1)
          this.items.delete(key)
          resolve()
        },
      }
      this.items.set(key, item)
      update(0)
    })
  }

  has(key) {
    return this.items.has(key)
  }

  step() {
    const now = this.clock.now()
    for (const item of [...this.items.values()]) {
      const t = Math.min(1, (now - item.start) / item.duration)
      if (t >= 1) item.finish(true)
      else item.update(item.easing(t))
    }
  }
}
