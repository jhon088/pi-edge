/**
 * The customer's own camera, shown back to them at a self-checkout.
 *
 * It is a mirror and nothing else. Nothing is recorded, nothing is stored,
 * no frame is read, no still is taken, and no byte reaches the core, the
 * daemon, the database or the network. The stream exists between the camera
 * and one `<video>` element in this window, and it ends when the element does.
 *
 * Off unless an operator turned it on. A kiosk that asks for camera permission
 * because it happens to be installed on a laptop with a webcam is a kiosk
 * nobody trusts, so `shouldStream` is consulted before anything is requested —
 * and it says no for the restaurant, always.
 */

/** What the preview is doing, and what Admin reports. */
export type CameraStatus =
  | "off"
  | "starting"
  | "live"
  /** The customer or the operating system said no. Nothing is retried. */
  | "denied"
  /** No camera, or the browser offers no way to ask. */
  | "unavailable";

/** Just enough of a track to stop it. Keeps the tests free of a real camera. */
export interface StoppableTrack {
  stop(): void;
}

export interface PreviewStream {
  getTracks(): StoppableTrack[];
}

/** How a stream is obtained. Injected, so nothing here is bound to a browser. */
export type OpenCamera = (constraints: {
  video: boolean | MediaTrackConstraints;
  audio: false;
}) => Promise<PreviewStream>;

/**
 * Whether a camera may be opened at all.
 *
 * Two conditions, both necessary: an operator enabled it, and this is the lane
 * that has a preview. Asked before every request, so switching to the
 * restaurant cannot leave one running.
 */
export function shouldStream(enabled: boolean, vertical: string): boolean {
  return enabled && vertical === "RETAIL";
}

/**
 * Video only, and no particular camera.
 *
 * `facingMode: "user"` is a preference, not a requirement — a kiosk with one
 * fixed camera has no front or back — so it is expressed as `ideal` and the
 * browser's default input is accepted when nothing matches. There is no device
 * id here: picking a specific camera is a setting nobody has asked for, and
 * hard-coding one would be worse than choosing none.
 */
export const PREVIEW_CONSTRAINTS = {
  video: {
    facingMode: { ideal: "user" },
    width: { ideal: 640 },
    height: { ideal: 480 },
  },
  audio: false,
} as const;

/**
 * One preview's worth of lifecycle.
 *
 * Holds at most one stream, stops every track it was given, and is safe to
 * stop twice — which matters, because it is stopped by an effect's cleanup, by
 * the operator switching it off, and by the lane changing, and any two of
 * those can happen in either order.
 */
export class CameraPreview {
  private stream: PreviewStream | null = null;
  private state: CameraStatus = "off";
  /** Bumped on every start, so a slow one cannot resurrect a stopped preview. */
  private generation = 0;
  private readonly open: OpenCamera | null;

  // Written out rather than declared as a constructor parameter property,
  // which is one of the few TypeScript constructs that emits code — and the
  // test runner strips types without compiling.
  constructor(open: OpenCamera | null) {
    this.open = open;
  }

  get status(): CameraStatus {
    return this.state;
  }

  get live(): boolean {
    return this.stream !== null;
  }

  /**
   * Ask for the camera, once.
   *
   * A refusal is an answer, not a failure: it is recorded for Admin to show
   * and the caller carries on. Nothing here ever throws, because the lane
   * behind it has to keep selling.
   */
  async start(): Promise<CameraStatus> {
    if (this.open === null) {
      this.state = "unavailable";
      return this.state;
    }
    if (this.stream !== null) {
      return this.state;
    }
    const mine = ++this.generation;
    this.state = "starting";
    try {
      const stream = await this.open(PREVIEW_CONSTRAINTS);
      if (mine !== this.generation) {
        // Stopped while the permission prompt was up. The stream that just
        // arrived belongs to nobody, so it is closed rather than kept.
        for (const track of stream.getTracks()) {
          track.stop();
        }
        return this.state;
      }
      this.stream = stream;
      this.state = "live";
    } catch (reason) {
      if (mine === this.generation) {
        this.state = refusal(reason);
      }
    }
    return this.state;
  }

  /** Stop every track and forget the stream. Safe with nothing running. */
  stop(): void {
    this.generation += 1;
    const stream = this.stream;
    this.stream = null;
    this.state = "off";
    if (!stream) {
      return;
    }
    for (const track of stream.getTracks()) {
      track.stop();
    }
  }

  /** The stream, for an element to render. Never read, never copied. */
  current(): PreviewStream | null {
    return this.stream;
  }
}

/** Why the camera did not open, in the two words Admin can act on. */
function refusal(reason: unknown): CameraStatus {
  const name =
    typeof reason === "object" && reason !== null && "name" in reason
      ? String((reason as { name: unknown }).name)
      : "";
  return name === "NotAllowedError" || name === "SecurityError"
    ? "denied"
    : "unavailable";
}

/** What Admin shows for each state. */
export function describeCamera(status: CameraStatus): string {
  switch (status) {
    case "live":
      return "Cámara activa";
    case "starting":
      return "Conectando con la cámara…";
    case "denied":
      return "Permiso de cámara denegado";
    case "unavailable":
      return "Cámara no disponible";
    case "off":
      return "Desactivada";
  }
}

/** The browser's camera, when this build is running in one that has it. */
export function browserCamera(): OpenCamera | null {
  if (typeof navigator === "undefined" || !navigator.mediaDevices?.getUserMedia) {
    return null;
  }
  return (constraints) =>
    navigator.mediaDevices.getUserMedia(
      constraints as MediaStreamConstraints,
    ) as Promise<PreviewStream>;
}
