/**
 * Trunk wasm initializer (M19): performance.mark around fetch + init so the
 * console phase line can split download vs instantiate vs Bevy startup.
 *
 * Contract: default-export a factory that returns { onStart, onProgress?, onComplete?, onSuccess?, onError? }.
 * @see https://trunkrs.dev/guide/advanced/initializer.html
 */
export default function () {
  const t0 = performance.now();
  return {
    onStart: () => {
      const boot = window.__zzBoot;
      if (boot && typeof boot.mark === "function") {
        boot.mark("wasm_fetch_start");
      } else {
        try {
          performance.mark("zz-wasm_fetch_start");
        } catch (_) {}
      }
      if (boot && typeof boot.setStatus === "function") {
        boot.setStatus("fetching wasm…");
        boot.setProgress(15, false);
      }
    },
    onProgress: ({ current, total }) => {
      const boot = window.__zzBoot;
      if (!boot) return;
      if (total > 0 && typeof boot.setProgress === "function") {
        const pct = 15 + Math.round((current / total) * 40);
        boot.setProgress(pct, true);
      }
      if (typeof boot.setStatus === "function") {
        if (total > 0) {
          const mb = (current / (1024 * 1024)).toFixed(1);
          const tmb = (total / (1024 * 1024)).toFixed(1);
          boot.setStatus(`loading ${mb} / ${tmb} MB`);
        } else {
          boot.setStatus("loading wasm…");
        }
      }
    },
    onComplete: () => {
      const boot = window.__zzBoot;
      if (boot && typeof boot.mark === "function") {
        boot.mark("wasm_fetch_done");
        if (boot.marks.wasm_fetch_start != null) {
          boot.recordPhase(
            "fetch_ms",
            Math.round(boot.marks.wasm_fetch_done - boot.marks.wasm_fetch_start)
          );
        }
      }
      if (boot && typeof boot.setStatus === "function") {
        boot.setStatus("instantiating…");
        boot.setProgress(55, true);
      }
      // init() is about to run (compile + start) — mark start of instantiate.
      if (boot && typeof boot.mark === "function") {
        boot.mark("wasm_init_start");
      }
    },
    onSuccess: () => {
      const boot = window.__zzBoot;
      if (boot && typeof boot.mark === "function") {
        boot.mark("wasm_init_done");
        if (boot.marks.wasm_init_start != null) {
          boot.recordPhase(
            "instantiate_ms",
            Math.round(boot.marks.wasm_init_done - boot.marks.wasm_init_start)
          );
        }
      }
      if (boot && typeof boot.setStatus === "function") {
        boot.setStatus("bevy starting…");
        boot.setProgress(65, true);
        boot.setHint("type a callsign — engine is booting");
      }
      // Note wall time from page for later first_frame delta.
      if (boot) {
        boot.recordPhase("page_to_init_ms", Math.round(performance.now() - t0));
      }
    },
    onError: (err) => {
      console.error("[zz boot] wasm init failed", err);
      const boot = window.__zzBoot;
      if (boot && typeof boot.setStatus === "function") {
        boot.setStatus("load failed");
        boot.setHint(String(err && err.message ? err.message : err));
      }
    },
  };
}
