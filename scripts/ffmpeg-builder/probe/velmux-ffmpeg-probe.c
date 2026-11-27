/*
 * velmux-ffmpeg-probe — generates the machine-readable capability manifest
 * from the ACTUAL FFmpeg libraries it is linked against.
 *
 * Output (stdout): JSON object
 * {
 *   "ffmpeg": "<version string>",
 *   "configuration": "<ffmpeg configure line>",
 *   "decoders":  [{"name": ..., "type": "video|audio|subtitle"} ...],
 *   "encoders":  [...],
 *   "muxers":    [...],
 *   "demuxers":  [...],
 *   "hwaccels":  [...]
 * }
 *
 * Build: linked with --static transitive deps via pkg-config.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavfilter/avfilter.h>
#include <libavutil/hwcontext.h>
#include <libavutil/avutil.h>

static const char *media_type_name(enum AVMediaType type) {
    switch (type) {
        case AVMEDIA_TYPE_VIDEO: return "video";
        case AVMEDIA_TYPE_AUDIO: return "audio";
        case AVMEDIA_TYPE_SUBTITLE: return "subtitle";
        case AVMEDIA_TYPE_DATA: return "data";
        case AVMEDIA_TYPE_ATTACHMENT: return "attachment";
        default: return "other";
    }
}

static void print_json_string(const char *s) {
    putchar('"');
    for (const char *p = s; *p; p++) {
        unsigned char c = (unsigned char)*p;
        if (c == '"' || c == '\\') {
            printf("\\%c", c);
        } else if (c < 0x20) {
            printf("\\u%04x", c);
        } else {
            putchar(c);
        }
    }
    putchar('"');
}

typedef void (*iterate_fn)(void **opaque, void *iter);

/* Generic JSON list printer for av_codec_iterate / av_muxer_iterate /
 * av_demuxer_iterate based enumeration. selector picks the name field. */
static void print_codec_list(int encoders) {
    void *opaque = NULL;
    const AVCodec *codec = NULL;
    int first = 1;
    while ((codec = av_codec_iterate(&opaque)) != NULL) {
        int matches = encoders ? av_codec_is_encoder(codec) : av_codec_is_decoder(codec);
        if (!matches) continue;
        if (!first) putchar(',');
        first = 0;
        printf("{\"name\":");
        print_json_string(codec->name);
        printf(",\"type\":\"%s\"", media_type_name(codec->type));
        printf("}");
    }
}

static void print_format_list(int muxers) {
    void *opaque = NULL;
    const AVOutputFormat *ofmt = NULL;
    const AVInputFormat *ifmt = NULL;
    int first = 1;
    if (muxers) {
        while ((ofmt = av_muxer_iterate(&opaque)) != NULL) {
            if (!first) putchar(',');
            first = 0;
            printf("{\"name\":");
            print_json_string(ofmt->name);
            printf(",\"long_name\":");
            print_json_string(ofmt->long_name ? ofmt->long_name : "");
            printf("}");
        }
    } else {
        while ((ifmt = av_demuxer_iterate(&opaque)) != NULL) {
            if (!first) putchar(',');
            first = 0;
            printf("{\"name\":");
            print_json_string(ifmt->name);
            printf(",\"long_name\":");
            print_json_string(ifmt->long_name ? ifmt->long_name : "");
            printf("}");
        }
    }
}

static void print_filters(void) {
    void *opaque = NULL;
    const AVFilter *filter = NULL;
    int first = 1;
    while ((filter = av_filter_iterate(&opaque)) != NULL) {
        if (!first) putchar(',');
        first = 0;
        printf("{\"name\":");
        print_json_string(filter->name);
        putchar('}');
    }
}

static void print_hw_device_types(void) {
    enum AVHWDeviceType type = AV_HWDEVICE_TYPE_NONE;
    int first = 1;
    while ((type = av_hwdevice_iterate_types(type)) != AV_HWDEVICE_TYPE_NONE) {
        if (!first) putchar(',');
        first = 0;
        print_json_string(av_hwdevice_get_type_name(type));
    }
}

int main(void) {
    printf("{\"ffmpeg\":");
    print_json_string(av_version_info());
    printf(",\"configuration\":");
    print_json_string(avutil_configuration ? avutil_configuration() : "");
    printf(",\"decoders\":[");
    print_codec_list(0);
    printf("],\"encoders\":[");
    print_codec_list(1);
    printf("],\"muxers\":[");
    print_format_list(1);
    printf("],\"demuxers\":[");
    print_format_list(0);
    printf("],\"filters\":[");
    print_filters();
    printf("],\"hw_device_types\":[");
    print_hw_device_types();
    printf("]}\n");
    return 0;
}
