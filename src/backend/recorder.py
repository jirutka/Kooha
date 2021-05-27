import os
from subprocess import PIPE, Popen

from gi.repository import GObject, Gst, Gio

from kooha.backend.portal import Portal

Gst.init(None)


class Recorder(GObject.GObject):

    def __init__(self):

        self.portal = Portal()
        self.portal.connect('ready', self._on_portal_ready)
        self.settings = Gio.Settings('io.github.seadve.Kooha')

    def _on_portal_ready(self, portal):
        fd, node_id = self.portal.get_recording_info()
        speaker_source, mic_source = self._get_default_audio_sources()
        filename = f"Kooha-{time.strftime('%Y-%m-%d-%H:%M:%S', time.localtime())}"
        saving_location = os.path.join(
            self._get_saving_location(), f'{filename}.{self.video_format}'
        ).replace(' ', r'\ ')

        self.pipeline = Gst.parse_launch(f'pipewiresrc fd={fd} path={node_id} do-timestamp=true keepalive-time=1000 resend-last=true ! video/x-raw,max-framerate={framerate}/1 ! videoconvert ! queue ! vp8enc min_quantizer=10 max_quantizer=10 cpu-used=16 cq_level=13 deadline=1 static-threshold=100 threads=3 ! queue ! matroskamux name=mux ! filesink location={saving_location} pulsesrc device="{mic_source}" ! queue ! audiomixer name=mix ! vorbisenc ! queue ! mux. pulsesrc device="{speaker_source}" ! queue ! mix.')
        self.pipeline.set_state(Gst.State.PLAYING)

        self.record_bus = self.pipeline.get_bus()
        self.record_bus.add_signal_watch()
        self.handler_id = self.record_bus.connect('message', self._on_gst_message)

    def _on_gst_message(self, bus, message):
        t = message.type
        if t == Gst.MessageType.EOS:
            self.pipeline.set_state(Gst.State.NULL)
        elif t == Gst.MessageType.ERROR:
            err, debug = message.parse_error()
            print("Error: %s" % err, debug)

    def _get_default_audio_sources(self):
        pactl_output = Popen(
            'pactl info | tail -n +13 | cut -d" " -f3',
            shell=True,
            text=True,
            stdout=PIPE
        ).stdout.read().rstrip()
        device_list = pactl_output.split("\n")
        default_sink = f"{device_list[0]}.monitor"
        default_source = device_list[1]
        if default_sink == default_source:
            return default_sink, None
        return default_sink, default_source

    def _get_saving_location(self):
        saving_location = self.settings.get_string('saving-location')
        if saving_location == 'default':
            saving_location = GLib.get_user_special_dir(GLib.UserDirectory.DIRECTORY_VIDEOS)
            if not os.path.exists(saving_location):
                saving_location = GLib.get_home_dir()
        return saving_location

    def start(self):
        self.framerate = self.settings.get_int('video-frames')
        self.saving_location =
        self.video_format =

        self.portal.open()

    def pause(self):
        self.pipeline.set_state(Gst.State.PAUSED)

    def resume(self):
        self.pipeline.set_state(Gst.State.PLAYING)

    def stop(self):
        self.pipeline.set_state(Gst.State.NULL)
        self.record_bus.remove_watch()
        self.record_bus.disconnect(self.handler_id)

        self.portal.close()
