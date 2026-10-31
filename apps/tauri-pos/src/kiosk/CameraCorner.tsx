/**
 * The preview itself: one `<video>`, mirrored, in the corner of the lane.
 *
 * Deliberately thin. All of the lifecycle lives in `camera.ts`, which has no
 * React in it and is tested without a camera; this element starts it when the
 * lane says so, stops it on the way out, and shows the customer the result.
 *
 * `muted` and `playsInline` are not style: without them a browser will refuse
 * to autoplay, and the audio track is never requested in the first place.
 */
import { useEffect, useRef, useState } from "react";

import {
  CameraPreview,
  browserCamera,
  shouldStream,
  type CameraStatus,
  type OpenCamera,
  type PreviewStream,
} from "./camera";

export function CameraCorner({
  enabled,
  hidden = false,
  open,
  onStatus,
}: {
  enabled: boolean;
  /**
   * Out of sight on screens it would sit on top of — the product grid, where
   * a corner panel would cover something for sale. The stream is **not**
   * stopped: the customer is still here, and restarting a camera every time
   * they look something up would flicker for no reason.
   */
  hidden?: boolean;
  /** Injected in tests; the browser's camera otherwise. */
  open?: OpenCamera | null;
  onStatus?: (status: CameraStatus) => void;
}) {
  const video = useRef<HTMLVideoElement | null>(null);
  const [status, setStatus] = useState<CameraStatus>("off");
  /**
   * The stream, held in state rather than written straight to the element.
   *
   * This is the whole reason the preview used to come up blank: `start()`
   * resolves, the status is set, and the `<video>` does not exist yet —
   * React has not re-rendered. Assigning `srcObject` in that same tick wrote
   * to a `null` ref and the stream was never attached. So the stream becomes
   * state, the element renders, and a second effect connects the two.
   */
  const [stream, setStream] = useState<PreviewStream | null>(null);

  useEffect(() => {
    if (!shouldStream(enabled)) {
      // Nothing is requested — so no permission is asked for — when the
      // preview is off or this is not a lane that has one.
      setStatus("off");
      setStream(null);
      onStatus?.("off");
      return;
    }
    const preview = new CameraPreview(open === undefined ? browserCamera() : open);
    let live = true;
    void preview.start().then((answer) => {
      if (!live) {
        // Unmounted while the permission prompt was up: `start` already
        // closed what arrived, and this only keeps the screen from reacting.
        return;
      }
      setStatus(answer);
      setStream(preview.current());
      onStatus?.(answer);
    });
    return () => {
      live = false;
      setStream(null);
      // Every way out ends here: disabled, switched lane, or the window
      // closing. Stopping the tracks is what puts the camera light out.
      preview.stop();
    };
  }, [enabled, open, onStatus]);

  // The element exists by now, because the render that created it is the one
  // that put the stream in state.
  useEffect(() => {
    const element = video.current;
    if (!element) {
      return;
    }
    element.srcObject = (stream as MediaStream | null) ?? null;
    if (stream) {
      // Autoplay is best-effort: a refusal leaves a still frame, not a crash.
      void element.play().catch(() => undefined);
    }
  }, [stream]);

  if (status !== "live" && status !== "starting") {
    // A refused or missing camera is not an error a customer should see: the
    // lane simply has no preview, and Admin is where the reason is reported.
    return null;
  }

  return (
    <div className="k-cam" aria-hidden hidden={hidden}>
      <video ref={video} className="k-cam__video" autoPlay muted playsInline />
    </div>
  );
}
