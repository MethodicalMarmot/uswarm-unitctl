import asyncio
import logging
import sys
import time

import uvloop
from blinker import signal
from pymavlink import mavutil
from pymavlink.dialects.v20.all import MAVLink_message
from pymavlink.mavutil import mavfile

LAT = 50.43931834072997
LON = 30.596333619878283
LAT = 50.42985445813565
LON = 30.42517038198206

config = {
    'mavlink': {
        'host': '0.0.0.0',
        'port': 14551,
        'protocol': 'udpin',
        'sniffer_sysid': 200,
        'iteration_period_ms': 10,

    }
}


class GpsHardcode:
    def __init__(self, config: dict):
        self._config = config
        self._mavlink: mavfile = None
        self._available_systems = set([])
        self._awaiting_messages = set([])

    async def run(self):
        logging.info('Starting mavlink sniffer connection')

        self.subscribe_message("HEARTBEAT", self._handle_heartbeat)

        while True:
            try:
                if self._mavlink is None:
                    logging.info(f'Trying mavlink connection')
                    self._mavlink = await asyncio.to_thread(lambda: mavutil.mavlink_connection(
                        f'{self._config["mavlink"]["protocol"]}:{self._config["mavlink"]["host"]}:{self._config["mavlink"]["port"]}',
                        source_system=int(self._config["mavlink"]["sniffer_sysid"]),
                        autoreconnect=True,
                        input=False
                    ))

                    logging.info(f'Mavlink connection established. Waiting for heartbeat...')
                    await asyncio.to_thread(lambda: self._mavlink.wait_heartbeat())

                    logging.info(f'Mavlink connected')

                elif self.awaiting_messages() is not None:
                    msg = await asyncio.to_thread(lambda: self._mavlink.recv_match(type=self.awaiting_messages(), blocking=True))
                    await self.received_message(msg)

                await asyncio.sleep(int(self._config['mavlink']['iteration_period_ms']) / 1000)
            except Exception as e:
                logging.error(f'Error connecting to mavlink: {e}')
                self._mavlink = None
                await asyncio.sleep(1)

    def subscribe_message(self, msg_type: str, handler: callable):
        signal(msg_type).connect(handler)
        self._awaiting_messages.add(msg_type)

    async def heartbeat(self):
        await self.get_fc_system_id()
        while True:
            await asyncio.to_thread(lambda: self._send_heartbeat())
            await asyncio.sleep(1)

    async def send_gps(self):
        await self.get_fc_system_id()
        while True:
            await asyncio.to_thread(lambda: self._send_gps())
            await asyncio.sleep(0.1)

    def awaiting_messages(self) -> set[str]:
        return self._awaiting_messages

    async def received_message(self, msg: MAVLink_message = None):
        msg_type = msg.get_type()
        if msg_type in self._awaiting_messages:
            await signal(msg_type).send_async(msg)

    def _send_heartbeat(self):
        if self._mavlink is not None:
            try:
                self._mavlink.mav.heartbeat_send(
                    mavutil.mavlink.MAV_TYPE_ONBOARD_CONTROLLER,
                    mavutil.mavlink.MAV_AUTOPILOT_INVALID,
                    0,
                    0,
                    0
                )
            except Exception as e:
                logging.error(f'Error sending heartbeat: {e}')
                self._mavlink = None

    def _send_gps(self):
        if self._mavlink is not None:
            try:
                logging.info('Sending GPS')
                # Target position
                lat = int(LAT * 1e7)  # latitude * 1E7
                lon = int(LON * 1e7)  # longitude * 1E7

                ignore_flags = mavutil.mavlink.GPS_INPUT_IGNORE_FLAG_VEL_HORIZ | \
                               mavutil.mavlink.GPS_INPUT_IGNORE_FLAG_VEL_VERT | \
                               mavutil.mavlink.GPS_INPUT_IGNORE_FLAG_ALT | \
                               mavutil.mavlink.GPS_INPUT_IGNORE_FLAG_SPEED_ACCURACY | \
                               mavutil.mavlink.GPS_INPUT_IGNORE_FLAG_HORIZONTAL_ACCURACY | \
                               mavutil.mavlink.GPS_INPUT_IGNORE_FLAG_VERTICAL_ACCURACY

                self._mavlink.mav.gps_input_send(
                    int(time.time() * 1e6),  # timestamp (usec)
                    2,  # gps_id
                    ignore_flags,  # ignore_flags
                    0,  # time_week_ms
                    0,  # time_week
                    2,  # fix_type (3D)
                    lat,  # lat (degE7)
                    lon,  # lon (degE7)
                    0,  # alt (m)
                    65535,  # HDOP
                    65535,  # VDOP
                    0.0,  # vn (m/s)
                    0.0,  # ve (m/s)
                    0.0,  # vd (m/s)
                    0.0,  # speed accuracy
                    0.0,  # horiz accuracy
                    0.0,  # vert accuracy
                    15,  # gps_satellites_visible
                )
            except Exception as e:
                logging.error(f'Error sending GPS: {e}')
                self._mavlink = None

    async def _handle_heartbeat(self, msg):
        logging.debug(f'Heartbeat from system (system {msg.get_srcSystem()} component {msg.get_srcComponent()})')
        self.available_systems(msg.get_srcSystem())

    async def get_fc_system_id(self):
        while True:
            valid_systems = [sys_id for sys_id in self.available_systems() if sys_id < 200]
            if valid_systems:
                return min(valid_systems)
            await asyncio.sleep(1)

    def available_systems(self, sys_id: int = None) -> set[int]:
        if sys_id is not None:
            self._available_systems.add(sys_id)

        return self._available_systems


async def main(args):
    severity = logging.DEBUG
    root = logging.getLogger()
    root.setLevel(severity)

    handler = logging.StreamHandler(sys.stdout)
    handler.setLevel(severity)
    formatter = logging.Formatter('%(asctime)s - %(module)s - %(levelname)s - %(message)s')
    handler.setFormatter(formatter)
    root.addHandler(handler)

    gps = GpsHardcode(config)
    await asyncio.gather(
        gps.run(),
        gps.heartbeat(),
        gps.send_gps(),
    )


if __name__ == "__main__":
    try:
        if sys.version_info >= (3, 11):
            with asyncio.Runner(loop_factory=uvloop.new_event_loop) as runner:
                runner.run(main([]))
        else:
            uvloop.install()
            asyncio.run(main([]))
    except KeyboardInterrupt:
        logging.info("Interrupted. Exiting...")
