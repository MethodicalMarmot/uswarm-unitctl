#!/bin/bash

[ -z "${GCS_IP}" ] && { echo "GCS_IP is not set" ; exit 1 ; }
[ -z "${REMOTE_VIDEO_PORT}" ] && { echo "REMOTE_VIDEO_PORT is not set" ; exit 1 ; }
[ -z "${CAMERA_WIDTH}" ] && { echo "CAMERA_WIDTH is not set" ; exit 1 ; }
[ -z "${CAMERA_HEIGHT}" ] && { echo "CAMERA_HEIGHT is not set" ; exit 1 ; }
[ -z "${CAMERA_FRAMERATE}" ] && { echo "CAMERA_FRAMERATE is not set" ; exit 1 ; }
[ -z "${CAMERA_BITRATE}" ] && { echo "CAMERA_BITRATE is not set" ; exit 1 ; }
[ -z "${CAMERA_FLIP}" ] && { echo "CAMERA_FLIP is not set" ; exit 1 ; }
[ -z "${CAMERA_TYPE}" ] && { echo "CAMERA_TYPE is not set" ; exit 1 ; }
[ -z "${CAMERA_DEVICE}" ] && { echo "CAMERA_DEVICE is not set" ; exit 1 ; }

SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

cd ${SCRIPT_DIR}

rpi() {
  if [ ${CAMERA_FLIP} == "1" ]; then
    CAMERA_FLIP="--hflip --vflip"
  else
    CAMERA_FLIP=""
  fi

  CAMERA_BIN=$(which /usr/bin/libcamera-vid || which /usr/bin/rpicam-vid)
  [ -f "${CAMERA_BIN}" ] || { >&2 echo "No rpi camera app found" ; exit 1 ; }

  ${CAMERA_BIN} -v 0 -t 0 ${CAMERA_FLIP} --flush --codec h264 --denoise off --nopreview --low-latency 1 --libav-video-codec h264_v4l2m2m -o - \
      --width ${CAMERA_WIDTH} \
      --height ${CAMERA_HEIGHT} \
      --bitrate ${CAMERA_BITRATE} \
      --framerate ${CAMERA_FRAMERATE} | \
  gst-launch-1.0 --quiet -v fdsrc ! \
      h264parse ! \
      rtph264pay config-interval=1 pt=96 mtu=200 aggregate-mode="zero-latency" ! \
      queue leaky=downstream max-size-buffers=1 max-size-time=0 max-size-bytes=0 ! \
      udpsink host=${GCS_IP} port=${REMOTE_VIDEO_PORT} sync=false
}

usb() {
  source ../venv-camera/bin/activate

  if [ ${CAMERA_FLIP} == "1" ]; then
    VIDEOFLIP=" videoflip video-direction=180 !"
  else
    VIDEOFLIP=""
  fi

  if (( CAMERA_WIDTH == 640 )); then
    CAMERA_HEIGHT=480
  fi

  CAMERA_BITRATE="$(echo "scale=0 ; ${CAMERA_BITRATE} / 1000" | bc)"

  echo v4l2src device=${CAMERA_DEVICE} ! \
    image/jpeg, width=${CAMERA_WIDTH}, height=${CAMERA_HEIGHT}, framerate=${CAMERA_FRAMERATE}/1 ! \
    jpegdec ! \
    videoconvert ! \
    video/x-raw, format=NV12 ! ${VIDEOFLIP} \
    x264enc bitrate=${CAMERA_BITRATE} speed-preset=ultrafast tune=zerolatency key-int-max=120 ! \
    identity name=id signal-handoffs=true ! \
    h264parse ! \
    queue max-size-buffers=1 flush-on-eos=true ! \
    rtph264pay config-interval=1 pt=96 mtu=1200 aggregate-mode=zero-latency ! \
    queue min-threshold-bytes=1200 ! \
    udpsink host=${GCS_IP} port=${REMOTE_VIDEO_PORT} | python usb-camera.py
}

usb_yuy2() {
  source ../venv-camera/bin/activate

  if [ ${CAMERA_FLIP} == "1" ]; then
    VIDEOFLIP=" videoflip video-direction=180 !"
  else
    VIDEOFLIP=""
  fi

  echo v4l2src device=${CAMERA_DEVICE} io-mode=2 ! \
      video/x-raw,format=YUY2,width=640,height=512,framerate=30/1,colorimetry=2:4:16:1 ! ${VIDEOFLIP} \
      videoconvert ! \
      x264enc tune=zerolatency bitrate=1000 speed-preset=ultrafast key-int-max=30 ! \
      identity name=id signal-handoffs=true ! \
      'video/x-h264,profile=baseline,level=(string)4' ! \
      queue max-size-time=0 max-size-bytes=0 max-size-buffers=0 flush-on-eos=true ! \
      rtph264pay config-interval=1 pt=96 mtu=1200 aggregate-mode="zero-latency" ! \
      queue min-threshold-bytes=1200 ! \
      udpsink host=${GCS_IP} port=${REMOTE_VIDEO_PORT} sync=false | python usb-camera.py
}

siyi() {
  [ -z "${CAMERA_IP}" ] && { echo "CAMERA_IP is not set" ; exit 1 ; }
  CAMERA_URL=rtsp://${CAMERA_IP}:8554/main.264

  if [ ${CAMERA_FLIP} == "1" ]; then
    VIDEOFLIP=" videoflip video-direction=180 !"
  else
    VIDEOFLIP=""
  fi

  if [ "${CAMERA_FRAMERATE}" == "60" ]; then
    CAMERA_BITRATE="$(echo "scale=0 ; ${CAMERA_BITRATE} / 3 / 1000" | bc)"
  elif [ "${CAMERA_FRAMERATE}" == "30" ]; then
    CAMERA_BITRATE="$(echo "scale=0 ; ${CAMERA_BITRATE} / 1000" | bc)"
  elif [ "${CAMERA_FRAMERATE}" == "20" ]; then
    CAMERA_BITRATE="$(echo "scale=0 ; ${CAMERA_BITRATE} / 1000" | bc)"
    VIDEORATE=" videorate max-rate=${CAMERA_FRAMERATE} !"
  else
    echo "Invalid framerate"
    exit 1
  fi

  gst-launch-1.0 --quiet -v rtspsrc location=${CAMERA_URL} latency=50 drop-on-latency=true buffer-mode=synced ! \
      application/x-rtp,media=video ! \
      rtph264depay ! \
      h264parse ! \
      avdec_h264 ! \
      videoscale ! "video/x-raw,width=${CAMERA_WIDTH},height=${CAMERA_HEIGHT},framerate=30/1" ! ${VIDEORATE} ${VIDEOFLIP} \
      x264enc speed-preset=ultrafast tune=zerolatency bitrate=${CAMERA_BITRATE} ! \
      rtph264pay config-interval=1 pt=96 mtu=1200 aggregate-mode="zero-latency" ! \
      queue min-threshold-bytes=1200 ! \
      udpsink host=${GCS_IP} port=${REMOTE_VIDEO_PORT} sync=false
}

openipc() {
  #TODO quality switching is not supported for openipc, move the whole func to the separate service
  while [ -z "$camera_ip" ]; do
    camera_ip=$(sudo cat /var/lib/NetworkManager/dnsmasq-eth0.leases | grep openipc | cut -d' ' -f3)
    if [ -z "$camera_ip" ]; then
      echo "Waiting for camera"
      sleep 1
    fi
  done

  echo "Forwarding video to ${GCS_IP}:${REMOTE_VIDEO_PORT}"
  sshpass -p 12345 ssh -o 'StrictHostKeyChecking=no' root@$camera_ip "cli -s .outgoing.server udp://${GCS_IP}:${REMOTE_VIDEO_PORT}" && \
    sshpass -p 12345 ssh -o 'StrictHostKeyChecking=no' root@$camera_ip 'killall -HUP majestic' && \
    break
}

fake() {
  gst-launch-1.0 -q \
    videotestsrc is-live=true pattern=ball ! \
    video/x-raw,width=${CAMERA_WIDTH},height=${CAMERA_HEIGHT},framerate=${CAMERA_FRAMERATE}/1 ! \
    clockoverlay valignment=top halignment=left ! \
    timeoverlay valignment=bottom halignment=left ! \
    videoconvert ! \
    x264enc tune=zerolatency speed-preset=ultrafast \
            bitrate=$((CAMERA_BITRATE / 1000)) key-int-max=30 ! \
    rtph264pay config-interval=1 pt=96 mtu=1200 aggregate-mode=zero-latency ! \
    udpsink host=${GCS_IP} port=${REMOTE_VIDEO_PORT} sync=false
}

case "${CAMERA_TYPE}" in
  "rpi")
  rpi
    ;;
  "usb")
  usb
    ;;
  "usb_yuy2")
  usb_yuy2
    ;;
  "openipc")
  openipc
    ;;
  "siyi")
  siyi
    ;;
  "fake")
  fake
    ;;
  *)
    echo "Invalid camera type"
    exit 1
    ;;
esac
