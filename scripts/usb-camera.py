import asyncio
import logging
import os
import sys
import time

import gi
import uvloop

severity = logging.INFO
root = logging.getLogger()
root.setLevel(severity)
handler = logging.StreamHandler(sys.stdout)
handler.setLevel(severity)
formatter = logging.Formatter('%(asctime)s - %(module)s - %(levelname)s - %(message)s')
handler.setFormatter(formatter)
root.addHandler(handler)

gi.require_version("Gst", "1.0")
from gi.repository import Gst, GObject

Gst.init(None)

NO_BANDWIDTH_TIMEOUT_S = 2.0

class PipelineManager:
    def __init__(self, pipeline_str):
        self.pipeline_str = pipeline_str
        self.pipeline = None
        self.identity = None
        self.total_bytes = 0
        self.last_handoff_time = time.time()
        self.last_bandwidth_time = time.time()
        self.lock = asyncio.Lock()
        self.restart_count = 0  # Track consecutive restarts

    def on_handoff(self, identity, buffer):
        self.total_bytes += buffer.get_size()
        now = time.time()
        self.last_handoff_time = now
        if now - self.last_bandwidth_time >= 1.0:
            mbps = (self.total_bytes * 8) / (now - self.last_bandwidth_time) / 1_000_000
            logging.debug(f"Approx. bandwidth: {mbps:.2f} Mbps")
            self.total_bytes = 0
            self.last_bandwidth_time = now
        self.restart_count = 0  # Reset on successful handoff

    def on_bus_message(self, bus, message):
        t = message.type
        if t == Gst.MessageType.ERROR:
            err, debug = message.parse_error()
            logging.error(f"GStreamer Error: {err}, Debug: {debug}")

    def start(self):
        self.pipeline = Gst.parse_launch(self.pipeline_str)
        bus = self.pipeline.get_bus()
        bus.add_signal_watch()
        bus.connect("message", self.on_bus_message)
        # Ensure pipeline is a Gst.Pipeline or Gst.Bin before calling get_by_name
        if hasattr(self.pipeline, 'get_by_name'):
            self.identity = self.pipeline.get_by_name("id")
            if self.identity:
                self.identity.connect("handoff", self.on_handoff)
        self.pipeline.set_state(Gst.State.PLAYING)
        logging.info("Pipeline started.")

    def stop(self):
        if self.pipeline:
            self.pipeline.set_state(Gst.State.NULL)
            logging.info("Pipeline stopped.")
            self.pipeline = None
            self.identity = None

    async def restart(self):
        async with self.lock:
            self.stop()
            await asyncio.sleep(0.5)  # Give GStreamer time to clean up
            self.start()
            self.last_handoff_time = time.time()
            self.last_bandwidth_time = time.time()
            self.total_bytes = 0
            self.restart_count += 1  # Increment on restart
            logging.info(f"Pipeline restarted. Consecutive restarts: {self.restart_count}")

async def bandwidth_monitor(manager: PipelineManager):
    while True:
        await asyncio.sleep(0.5)
        now = time.time()
        # If no handoff in last 2 seconds, restart pipeline
        if now - manager.last_handoff_time > NO_BANDWIDTH_TIMEOUT_S:
            logging.warning(f"No bandwidth detected for {NO_BANDWIDTH_TIMEOUT_S} seconds. Restarting pipeline...")
            await manager.restart()
            if manager.restart_count >= 10:
                logging.error("10 consecutive pipeline restarts detected. Restarting app...")
                os.execv(sys.executable, [sys.executable] + sys.argv)

# pipeline_str = (
#     "v4l2src device=/dev/video0 io-mode=2 ! "
#     "video/x-raw,format=YUY2,width=640,height=512,framerate=30/1,colorimetry=2:4:16:1 ! "
#     "videoflip video-direction=180 ! "
#     "videoconvert ! "
#     "x264enc tune=zerolatency bitrate=2500 speed-preset=ultrafast key-int-max=30 ! "
#     "identity name=id signal-handoffs=true ! "
#     "video/x-h264,profile=baseline,level=(string)4 ! "
#     "queue max-size-time=0 max-size-bytes=0 max-size-buffers=0 flush-on-eos=true ! "
#     "rtph264pay config-interval=1 pt=96 mtu=1200 aggregate-mode=zero-latency ! "
#     "queue min-threshold-bytes=1200 ! "
#     "udpsink host=10.45.0.1 port=5600 sync=false"
# )

async def main():
    # Read pipeline_str from stdin
    pipeline_str = sys.stdin.read().strip()
    manager = PipelineManager(pipeline_str)
    manager.start()
    monitor_task = asyncio.create_task(bandwidth_monitor(manager))
    # Integrate GObject main loop with asyncio
    loop = asyncio.get_event_loop()
    gobject_loop = GObject.MainLoop()
    def run_gobject():
        gobject_loop.run()
    await loop.run_in_executor(None, run_gobject)
    await monitor_task

if __name__ == "__main__":
    try:
        if sys.version_info >= (3, 11):
            with asyncio.Runner(loop_factory=uvloop.new_event_loop) as runner:
                runner.run(main())
        else:
            uvloop.install()
            asyncio.run(main())
    except KeyboardInterrupt:
        logging.info("Interrupted. Exiting...")
