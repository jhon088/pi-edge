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
import { signAdopted } from "./flow";
import { BRANDS, brandOf } from "./brands";
import { Splash } from "./components";
import KioskApp from "./KioskApp";
import RetailApp from "./RetailApp";
import type { KioskPreferences } from "./types";
import "./kiosk.css";

/**
 * How often the screen asks the host whether the sign on the door changed.
 *
 * The sign is the one preference somebody can flip while a screen is live and
 * from somewhere the screen cannot see: Admin writes the file, and so does a
 * floor app over the control route. Neither has a way to reach this webview,
 * so the webview asks. The host answers out of a small file it already reads
 * on every call — this is a showroom floor, not a hot path.
 */
const SIGN_POLL_MS = 1_500;

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
          branch_id: "",
          kiosk_id: "",
          cashier_id: "",
          kiosk_open: true,
          cashgenic: {
            enabled: false,
            host: "",
            port: 44333,
            username: "",
            password: "",
          },
          // The quiet defaults: no PIN, and not listening for a control POST.
          admin_pin: "",
          remote_control: false,
        }),
      );
  }, []);

  useEffect(read, [read]);

  // The sign can come down from Admin or from a floor app, and until this
  // existed the screen never found out: `read` ran once on mount and again
  // only on the way out of Admin, which is why a local close appeared to work
  // and a remote one did not. Nothing else about the lane is taken from here
  // — see `signAdopted`.
  useEffect(() => {
    const timer = window.setInterval(() => {
      void preferences()
        .then((fresh) => setLane((held) => signAdopted(held, fresh)))
        // A host that did not answer has said nothing. The screen keeps the
        // sign it has and the next read asks again.
        .catch(() => {});
    }, SIGN_POLL_MS);
    return () => window.clearInterval(timer);
  }, []);

  // The palette is a document-level attribute rather than a wrapper class, so
  // fixed-position furniture — the camera corner, the admin key, a toast —
  // picks the brand up too.
  useEffect(() => {
    const theme = lane ? brandOf(lane.vertical).theme : "px";
    document.documentElement.setAttribute("data-brand", theme);
    return () => document.documentElement.removeAttribute("data-brand");
  }, [lane]);

  if (!lane) {
    return (
      <div className="k-app">
        <Splash message="Preparando el equipo…" />
      </div>
    );
  }

  const brand = brandOf(lane.vertical);
  if (brand.retail) {
    return (
      <RetailApp
        key={lane.vertical}
        channel={lane.channel_ref}
        cameraPreview={lane.camera_preview}
        kioskOpen={lane.kiosk_open}
        cashGenic={lane.cashgenic.enabled}
        onLeave={read}
      />
    );
  }
  return (
    <KioskApp
      key={lane.vertical}
      channel={lane.channel_ref}
      kioskOpen={lane.kiosk_open}
      cashGenic={lane.cashgenic.enabled}
      cameraPreview={lane.camera_preview}
      invitation={brand.invitation ?? BRANDS[0]!.invitation!}
      onLeave={read}
    />
  );
}
