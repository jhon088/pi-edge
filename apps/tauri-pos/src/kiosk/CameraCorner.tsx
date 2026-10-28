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
} from "./camera";

export function CameraCorner({
  enabled,
  vertical,
  hidden = false,
  open,
  onStatus,
}: {
  enabled: boolean;
  /** The lane this screen is. Only retail has a preview. */
  vertical: string;
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

  useEffect(() => {
    if (!shouldStream(enabled, vertical)) {
      // Nothing is requested — so no permission is asked for — when the
      // preview is off or this is not the lane that has one.
      setStatus("off");
      onStatus?.("off");
      return;
    }
    const preview = new CameraPreview(open === undefined ? browserCamera() : open);
    let live = true;
    void preview.start().then((answer) => {
      if (!live) {
        // Unmounted while the permission prompt was up: `start` already closed
        // what arrived, and this only keeps the screen from reacting to it.
        return;
      }
      setStatus(answer);
      onStatus?.(answer);
      const element = video.current;
      const stream = preview.current();
      if (element && stream) {
        element.srcObject = stream as unknown as MediaStream;
      }
    });
    return () => {
      live = false;
      // Every way out of this lane ends here: disabled, switched to the
      // restaurant, or the window closing. Stopping the tracks is what puts
      // the camera light out; the element is being unmounted with it, so
      // there is nothing to detach the stream from.
      preview.stop();
    };
  }, [enabled, vertical, open, onStatus]);

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
