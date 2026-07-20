// DOM HUD: health, timer, score, kill feed, hit marker, banners.

export class Hud {
  private timer = document.getElementById("hud-timer")!;
  private score = document.getElementById("hud-score")!;
  private health = document.getElementById("hud-health")!;
  private killfeed = document.getElementById("killfeed")!;
  private hitmarker = document.getElementById("hitmarker")!;
  private banner = document.getElementById("banner")!;
  private root = document.getElementById("hud")!;
  private hitTimeout: ReturnType<typeof setTimeout> | null = null;

  show(): void {
    this.root.classList.remove("hidden");
  }
  hide(): void {
    this.root.classList.add("hidden");
    this.killfeed.innerHTML = "";
    this.setBanner(null);
  }

  setTimeLeft(ms: number, suddenDeath = false): void {
    if (suddenDeath) {
      this.timer.textContent = "SUDDEN DEATH";
      return;
    }
    const s = Math.max(0, Math.ceil(ms / 1000));
    this.timer.textContent = `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
  }

  setScore(text: string): void {
    this.score.textContent = text;
  }

  setHealth(hp: number, max: number): void {
    this.health.style.width = `${Math.max(0, (hp / max) * 100)}%`;
  }

  feed(html: string): void {
    const el = document.createElement("div");
    el.className = "kf";
    el.innerHTML = html;
    this.killfeed.prepend(el);
    while (this.killfeed.children.length > 4) this.killfeed.lastChild?.remove();
    setTimeout(() => el.remove(), 5000);
  }

  hitMarker(): void {
    this.hitmarker.classList.remove("hidden");
    if (this.hitTimeout) clearTimeout(this.hitTimeout);
    this.hitTimeout = setTimeout(() => this.hitmarker.classList.add("hidden"), 120);
  }

  setBanner(html: string | null): void {
    if (html === null) {
      this.banner.classList.add("hidden");
      this.banner.innerHTML = "";
    } else {
      this.banner.classList.remove("hidden");
      this.banner.innerHTML = html;
    }
  }
}
