export interface DesktopUpdate {
  version: string;
  downloadAndInstall: () => Promise<void>;
  close: () => Promise<void>;
}

export type DesktopUpdateState =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "none" }
  | { kind: "available"; version: string }
  | { kind: "installing" }
  | { kind: "handed-off" }
  | { kind: "error"; message: string };

// Own each native update resource until superseded, disposed, or handed off.
// Installing on Windows exits the app; resolved install isn't runtime proof.
export class DesktopUpdateSession {
  private update: DesktopUpdate | null = null;
  private busy = false;
  private disposed = false;
  private installing = false;
  private readonly checkUpdate: () => Promise<DesktopUpdate | null>;
  private readonly publish: (state: DesktopUpdateState) => void;

  constructor(
    checkUpdate: () => Promise<DesktopUpdate | null>,
    publish: (state: DesktopUpdateState) => void,
  ) {
    this.checkUpdate = checkUpdate;
    this.publish = publish;
  }

  private emit(state: DesktopUpdateState) {
    if (this.disposed) return;
    try { this.publish(state); } catch { /* Observers cannot wedge native ownership. */ }
  }
  private async release(update: DesktopUpdate | null) {
    // A failed close cannot convert a failed check into a success.
    try { await update?.close(); } catch { /* Native process owns final cleanup. */ }
  }

  async check() {
    if (this.disposed || this.busy) return;
    this.busy = true;
    this.emit({ kind: "checking" });
    const previous = this.update;
    this.update = null;
    void this.release(previous);
    try {
      if (this.disposed) return;
      const update = await this.checkUpdate();
      if (this.disposed) { void this.release(update); return; }
      this.update = update;
      this.emit(update ? { kind: "available", version: update.version } : { kind: "none" });
    } catch {
      this.emit({ kind: "error", message: "Update check failed. Try again when connected." });
    } finally { this.busy = false; }
  }

  async install() {
    if (this.disposed || this.busy || !this.update) return;
    this.busy = true;
    this.installing = true;
    const update = this.update;
    this.emit({ kind: "installing" });
    try {
      await update.downloadAndInstall();
      this.emit({ kind: "handed-off" });
    } catch {
      this.emit({ kind: "error", message: "Installation failed. The running app has not been verified as updated. Check again to retry." });
    } finally {
      this.update = null;
      this.installing = false;
      this.busy = false;
      void this.release(update);
    }
  }

  dispose() {
    this.disposed = true;
    // Do not close the handle while the native installer is using it.
    if (!this.installing) {
      const update = this.update;
      this.update = null;
      void this.release(update);
    }
  }
}
