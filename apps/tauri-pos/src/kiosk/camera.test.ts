/**
 * The camera preview, without a camera.
 *
 * Every test here injects its own `getUserMedia`, so what is being checked is
 * the kiosk's behaviour — when it asks, when it must not ask, and what it does
 * when the answer is no — rather than whether this machine has a webcam.
 */
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  CameraPreview,
  PREVIEW_CONSTRAINTS,
  describeCamera,
  shouldStream,
  type OpenCamera,
  type PreviewStream,
} from "./camera.ts";

/** A stream whose tracks remember being stopped. */
function fakeStream(tracks = 1) {
  const stopped: boolean[] = Array.from({ length: tracks }, () => false);
  const stream: PreviewStream = {
    getTracks: () =>
      stopped.map((_, index) => ({
        stop: () => {
          stopped[index] = true;
        },
      })),
  };
  return { stream, stopped: () => stopped.every(Boolean), count: () => stopped.length };
}

/** A camera that answers, and counts how many times it was asked. */
function camera(answer: () => Promise<PreviewStream>) {
  const asked: unknown[] = [];
  const open: OpenCamera = (constraints) => {
    asked.push(constraints);
    return answer();
  };
  return { open, asked };
}

function refusedWith(name: string) {
  const error = new Error("refused");
  error.name = name;
  return error;
}

describe("camera_disabled_does_not_request_media", () => {
  it("does not stream when nobody turned it on", () => {
    assert.equal(shouldStream(false), false);
  });

  it("asks for nothing at all while disabled", async () => {
    const { open, asked } = camera(async () => fakeStream().stream);
    const preview = new CameraPreview(open);
    // The shell consults the policy first; this is what it would skip.
    if (shouldStream(false)) {
      await preview.start();
    }
    assert.deepEqual(asked, [], "a disabled preview reached for the camera");
    assert.equal(preview.status, "off");
    assert.equal(preview.live, false);
  });
});

describe("the preview belongs to the box, not to a lane", () => {
  it("streams in every brand once it is turned on", () => {
    for (const lane of ["RESTAURANT", "TIM_HORTONS", "SBARRO", "RETAIL"]) {
      assert.equal(shouldStream(true), true, lane);
    }
  });

  it("streams in none of them while it is off", () => {
    for (const lane of ["RESTAURANT", "TIM_HORTONS", "SBARRO", "RETAIL"]) {
      assert.equal(shouldStream(false), false, lane);
    }
  });
});

describe("camera_enabled_requests_video", () => {
  it("asks for video, never audio", async () => {
    const { open, asked } = camera(async () => fakeStream().stream);
    const preview = new CameraPreview(open);
    assert.equal(await preview.start(), "live");
    assert.equal(asked.length, 1);
    assert.deepEqual(asked[0], PREVIEW_CONSTRAINTS);
    assert.equal(
      (PREVIEW_CONSTRAINTS as { audio: false }).audio,
      false,
      "the preview asked for a microphone",
    );
    assert.equal(preview.live, true);
  });

  it("asks once, however many times it is started", async () => {
    const { open, asked } = camera(async () => fakeStream().stream);
    const preview = new CameraPreview(open);
    await preview.start();
    await preview.start();
    await preview.start();
    assert.equal(asked.length, 1, "the permission prompt would reappear");
  });

  it("names no particular camera", () => {
    const video = PREVIEW_CONSTRAINTS.video as Record<string, unknown>;
    assert.equal("deviceId" in video, false, "the preview is bound to one camera");
  });
});

describe("camera_permission_failure_does_not_block_retail", () => {
  it("records a refusal instead of throwing", async () => {
    const { open } = camera(async () => {
      throw refusedWith("NotAllowedError");
    });
    const preview = new CameraPreview(open);
    assert.equal(await preview.start(), "denied");
    assert.equal(preview.live, false);
    assert.equal(describeCamera(preview.status), "Permiso de cámara denegado");
  });

  it("calls a missing camera unavailable, and carries on", async () => {
    const { open } = camera(async () => {
      throw refusedWith("NotFoundError");
    });
    const preview = new CameraPreview(open);
    assert.equal(await preview.start(), "unavailable");
    assert.equal(preview.live, false);
  });

  it("is unavailable, not broken, in a window with no camera API", async () => {
    const preview = new CameraPreview(null);
    assert.equal(await preview.start(), "unavailable");
    assert.equal(preview.live, false);
  });

  it("can be stopped after a refusal, and started again later", async () => {
    let allow = false;
    const { open } = camera(async () => {
      if (!allow) {
        throw refusedWith("NotAllowedError");
      }
      return fakeStream().stream;
    });
    const preview = new CameraPreview(open);
    assert.equal(await preview.start(), "denied");
    preview.stop();
    allow = true;
    assert.equal(await preview.start(), "live");
  });
});

describe("camera_stream_stops_on_disable", () => {
  it("stops every track it was given", async () => {
    const live = fakeStream(2);
    const { open } = camera(async () => live.stream);
    const preview = new CameraPreview(open);
    await preview.start();
    assert.equal(preview.live, true);

    preview.stop();
    assert.equal(live.stopped(), true, "the camera was left running");
    assert.equal(preview.live, false);
    assert.equal(preview.status, "off");
    assert.equal(preview.current(), null);
  });

  it("is safe to stop twice, and safe to stop having never started", () => {
    const preview = new CameraPreview(null);
    preview.stop();
    preview.stop();
    assert.equal(preview.status, "off");
  });

  it("does not keep a stream that arrived after it was stopped", async () => {
    // The permission prompt is modal and slow; the lane can change under it.
    let release: (stream: PreviewStream) => void = () => {};
    const pending = new Promise<PreviewStream>((resolve) => {
      release = resolve;
    });
    const { open } = camera(() => pending);
    const preview = new CameraPreview(open);
    const starting = preview.start();

    preview.stop();
    const late = fakeStream(2);
    release(late.stream);
    await starting;

    assert.equal(preview.live, false, "a stopped preview came back to life");
    assert.equal(late.stopped(), true, "the late stream was left running");
  });
});

describe("camera_stream_stops_when_the_operator_turns_it_off", () => {
  it("the policy turns it off, and the stream follows", async () => {
    const live = fakeStream();
    const { open } = camera(async () => live.stream);
    const preview = new CameraPreview(open);

    let enabled = true;
    if (shouldStream(enabled)) {
      await preview.start();
    }
    assert.equal(preview.live, true);

    // What the shell does when Admin switches it off: the lane is unchanged,
    // the preference is not, so the stream ends and the light goes out.
    enabled = false;
    if (!shouldStream(enabled)) {
      preview.stop();
    }
    assert.equal(live.stopped(), true, "the camera stayed on");
    assert.equal(preview.live, false);

    // And turning it back on starts a new one.
    enabled = true;
    const second = fakeStream();
    const again = new CameraPreview(camera(async () => second.stream).open);
    if (shouldStream(enabled)) {
      await again.start();
    }
    assert.equal(again.live, true, "switching it back on left the preview dark");
  });
});

describe("what Admin reports", () => {
  it("has a word for every state", () => {
    for (const status of ["off", "starting", "live", "denied", "unavailable"] as const) {
      assert.ok(describeCamera(status).length > 0, status);
    }
  });
});
