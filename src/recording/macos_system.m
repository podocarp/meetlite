#import <AudioToolbox/AudioToolbox.h>
#import <CoreAudio/CoreAudio.h>
#import <CoreAudio/AudioHardwareTapping.h>
#import <CoreAudio/CATapDescription.h>
#import <Foundation/Foundation.h>

typedef void (*MeetliteAudioCallback)(const float *samples, size_t sample_count, void *context);

typedef struct {
  AudioObjectID tap_device;
  AudioDeviceID aggregate_device;
  AudioDeviceIOProcID io_proc;
  MeetliteAudioCallback callback;
  void *context;
  Float64 sample_rate;
} MeetliteSystemAudioCapture;

static void set_error(char *buffer, size_t buffer_length, NSString *message) {
  if (buffer == NULL || buffer_length == 0) {
    return;
  }
  snprintf(buffer, buffer_length, "%s", message.UTF8String);
}

static NSString *status_message(NSString *operation, OSStatus status) {
  return [NSString stringWithFormat:@"%@ failed (OSStatus %d)", operation, (int)status];
}

static OSStatus audio_callback(AudioDeviceID device,
                               const AudioTimeStamp *now,
                               const AudioBufferList *input_data,
                               const AudioTimeStamp *input_time,
                               AudioBufferList *output_data,
                               const AudioTimeStamp *output_time,
                               void *client_data) {
  (void)device;
  (void)now;
  (void)input_time;
  (void)output_data;
  (void)output_time;
  MeetliteSystemAudioCapture *capture = client_data;
  if (capture == NULL || capture->callback == NULL || input_data == NULL) {
    return noErr;
  }

  for (UInt32 index = 0; index < input_data->mNumberBuffers; index++) {
    const AudioBuffer buffer = input_data->mBuffers[index];
    if (buffer.mData != NULL && buffer.mDataByteSize >= sizeof(float)) {
      capture->callback((const float *)buffer.mData,
                        buffer.mDataByteSize / sizeof(float),
                        capture->context);
    }
  }
  return noErr;
}

MeetliteSystemAudioCapture *meetlite_system_audio_start(MeetliteAudioCallback callback,
                                                         void *context,
                                                         char *error,
                                                         size_t error_length) {
  MeetliteSystemAudioCapture *capture = calloc(1, sizeof(MeetliteSystemAudioCapture));
  if (capture == NULL) {
    set_error(error, error_length, @"could not allocate system-audio capture state");
    return NULL;
  }

  AudioDeviceID output_device = kAudioObjectUnknown;
  AudioObjectPropertyAddress output_address = {
      kAudioHardwarePropertyDefaultOutputDevice,
      kAudioObjectPropertyScopeGlobal,
      kAudioObjectPropertyElementMain,
  };
  UInt32 size = sizeof(output_device);
  OSStatus status = AudioObjectGetPropertyData(kAudioObjectSystemObject, &output_address, 0, NULL,
                                               &size, &output_device);
  if (status != noErr) {
    set_error(error, error_length, status_message(@"getting default output device", status));
    free(capture);
    return NULL;
  }

  AudioObjectPropertyAddress uid_address = {
      kAudioDevicePropertyDeviceUID,
      kAudioObjectPropertyScopeGlobal,
      kAudioObjectPropertyElementMain,
  };
  CFStringRef output_uid = NULL;
  size = sizeof(output_uid);
  status = AudioObjectGetPropertyData(output_device, &uid_address, 0, NULL, &size, &output_uid);
  if (status != noErr || output_uid == NULL) {
    set_error(error, error_length, status_message(@"getting default output device UID", status));
    free(capture);
    return NULL;
  }

  CATapDescription *tap_description =
      [[CATapDescription alloc] initMonoGlobalTapButExcludeProcesses:@[]];
  status = AudioHardwareCreateProcessTap(tap_description, &capture->tap_device);
  if (status != noErr) {
    set_error(error, error_length,
              status_message(@"creating Core Audio process tap (grant Audio Capture permission)", status));
    CFRelease(output_uid);
    free(capture);
    return NULL;
  }

  AudioStreamBasicDescription tap_format = {0};
  AudioObjectPropertyAddress tap_format_address = {
      kAudioTapPropertyFormat,
      kAudioObjectPropertyScopeGlobal,
      kAudioObjectPropertyElementMain,
  };
  size = sizeof(tap_format);
  status = AudioObjectGetPropertyData(capture->tap_device, &tap_format_address, 0, NULL, &size,
                                      &tap_format);
  if (status != noErr) {
    set_error(error, error_length, status_message(@"getting process tap format", status));
    AudioHardwareDestroyProcessTap(capture->tap_device);
    CFRelease(output_uid);
    free(capture);
    return NULL;
  }
  if (tap_format.mFormatID != kAudioFormatLinearPCM ||
      (tap_format.mFormatFlags & kAudioFormatFlagIsFloat) == 0 ||
      tap_format.mChannelsPerFrame != 1 || tap_format.mBitsPerChannel != 32 ||
      tap_format.mBytesPerFrame != sizeof(float)) {
    set_error(error, error_length, @"Core Audio process tap did not provide mono float32 PCM");
    AudioHardwareDestroyProcessTap(capture->tap_device);
    CFRelease(output_uid);
    free(capture);
    return NULL;
  }

  AudioObjectPropertyAddress tap_uid_address = {
      kAudioTapPropertyUID,
      kAudioObjectPropertyScopeGlobal,
      kAudioObjectPropertyElementMain,
  };
  CFStringRef tap_uid = NULL;
  size = sizeof(tap_uid);
  status = AudioObjectGetPropertyData(capture->tap_device, &tap_uid_address, 0, NULL, &size,
                                      &tap_uid);
  if (status != noErr || tap_uid == NULL) {
    set_error(error, error_length, status_message(@"getting process tap UID", status));
    AudioHardwareDestroyProcessTap(capture->tap_device);
    CFRelease(output_uid);
    free(capture);
    return NULL;
  }

  NSDictionary *tap = @{ @kAudioSubTapUIDKey : (__bridge NSString *)tap_uid };
  NSDictionary *aggregate = @{
    @kAudioAggregateDeviceIsPrivateKey : @YES,
    @kAudioAggregateDeviceIsStackedKey : @NO,
    @kAudioAggregateDeviceTapAutoStartKey : @YES,
    @kAudioAggregateDeviceNameKey : @"meetlite-audio-tap",
    @kAudioAggregateDeviceMainSubDeviceKey : (__bridge NSString *)output_uid,
    @kAudioAggregateDeviceUIDKey : NSUUID.UUID.UUIDString,
    @kAudioAggregateDeviceTapListKey : @[ tap ],
  };
  status = AudioHardwareCreateAggregateDevice((__bridge CFDictionaryRef)aggregate,
                                               &capture->aggregate_device);
  CFRelease(tap_uid);
  CFRelease(output_uid);
  if (status != noErr) {
    set_error(error, error_length, status_message(@"creating aggregate device", status));
    AudioHardwareDestroyProcessTap(capture->tap_device);
    free(capture);
    return NULL;
  }

  status = AudioDeviceCreateIOProcID(capture->aggregate_device, audio_callback, capture,
                                     &capture->io_proc);
  if (status != noErr) {
    set_error(error, error_length, status_message(@"creating system-audio callback", status));
    AudioHardwareDestroyAggregateDevice(capture->aggregate_device);
    AudioHardwareDestroyProcessTap(capture->tap_device);
    free(capture);
    return NULL;
  }

  AudioObjectPropertyAddress rate_address = {
      kAudioDevicePropertyNominalSampleRate,
      kAudioObjectPropertyScopeGlobal,
      kAudioObjectPropertyElementMain,
  };
  size = sizeof(capture->sample_rate);
  status = AudioObjectGetPropertyData(capture->aggregate_device, &rate_address, 0, NULL, &size,
                                      &capture->sample_rate);
  if (status != noErr) {
    set_error(error, error_length, status_message(@"getting system-audio sample rate", status));
    AudioDeviceDestroyIOProcID(capture->aggregate_device, capture->io_proc);
    AudioHardwareDestroyAggregateDevice(capture->aggregate_device);
    AudioHardwareDestroyProcessTap(capture->tap_device);
    free(capture);
    return NULL;
  }

  capture->callback = callback;
  capture->context = context;
  status = AudioDeviceStart(capture->aggregate_device, capture->io_proc);
  if (status != noErr) {
    set_error(error, error_length, status_message(@"starting system-audio capture", status));
    AudioDeviceDestroyIOProcID(capture->aggregate_device, capture->io_proc);
    AudioHardwareDestroyAggregateDevice(capture->aggregate_device);
    AudioHardwareDestroyProcessTap(capture->tap_device);
    free(capture);
    return NULL;
  }

  return capture;
}

Float64 meetlite_system_audio_sample_rate(const MeetliteSystemAudioCapture *capture) {
  return capture == NULL ? 0 : capture->sample_rate;
}

void meetlite_system_audio_stop(MeetliteSystemAudioCapture *capture) {
  if (capture == NULL) {
    return;
  }
  if (capture->aggregate_device != kAudioObjectUnknown && capture->io_proc != NULL) {
    AudioDeviceStop(capture->aggregate_device, capture->io_proc);
    AudioDeviceDestroyIOProcID(capture->aggregate_device, capture->io_proc);
  }
  if (capture->aggregate_device != kAudioObjectUnknown) {
    AudioHardwareDestroyAggregateDevice(capture->aggregate_device);
  }
  if (capture->tap_device != kAudioObjectUnknown) {
    AudioHardwareDestroyProcessTap(capture->tap_device);
  }
  free(capture);
}
