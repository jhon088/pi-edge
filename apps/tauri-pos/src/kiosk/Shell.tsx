/**
 * Which lane this screen is.
 *
 * One application, one core, one catalog, two shells. The host owns the
 * choice — it is in a file in the data directory, not in the webview — so a
 * reload, a crash or a reinstall of the frontend comes back to the same lane
 * the shop was set up as.
 *
 * Mounting is keyed on the vertical, so switching lanes discards every bit of
 * screen state the previous one held rather than trying to translate it. The
 * sale is the core's and was already abandoned by the host on the way out.
 */
import { useCallback, useEffect, useState } from "react";

import { preferences } from "./api";
import { Splash } from "./components";
import KioskApp from "./KioskApp";
import RetailApp from "./RetailApp";
import type { KioskPreferences } from "./types";
import "./kiosk.css";

export default function Shell() {
  const [lane, setLane] = useState<KioskPreferences | null>(null);

  const read = useCallback(() => {
    void preferences()
      .then(setLane)
      // A host that cannot answer still has to open a working screen, and the
      // restaurant with no camera is the default this build ships as.
      .catch(() =>
        setLane({
          vertical: "RESTAURANT",
          channel_ref: "KIOSK",
          camera_preview: false,
        }),
      );
  }, []);

  useEffect(read, [read]);

  if (!lane) {
    return (
      <div className="k-app">
        <Splash message="Preparando el equipo…" />
      </div>
    );
  }

  if (lane.vertical === "RETAIL") {
    return (
      <RetailApp
        key={lane.vertical}
        channel={lane.channel_ref}
        cameraPreview={lane.camera_preview}
        onLeave={read}
      />
    );
  }
  return <KioskApp key={lane.vertical} onLeave={read} />;
}
