#include <freerdp/addin.h>
#include <freerdp/client.h>
#include <freerdp/client/channels.h>
#include <freerdp/client/cmdline.h>
#include <freerdp/client/disp.h>
#include <freerdp/gdi/gdi.h>
#include <freerdp/gdi/gfx.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
static DispClientContext *display;
static UINT caps(DispClientContext *d, UINT32 n, UINT32 a, UINT32 b) {
    display = d;
    DISPLAY_CONTROL_MONITOR_LAYOUT m = {.Flags = 1,
                                        .Width = 1024,
                                        .Height = 768,
                                        .DesktopScaleFactor = 100,
                                        .DeviceScaleFactor = 100};
    fprintf(stderr, "CONTROL: received caps %u %u %u, sending 1024x768\n", n, a, b);
    return d->SendMonitorLayout(d, 1, &m);
}
static pcRdpgfxResetGraphics on_reset;
static int portrait_reset, portrait_frame;
static pcRdpgfxEndFrame on_frame;
static UINT reset(RdpgfxClientContext *c, const RDPGFX_RESET_GRAPHICS_PDU *r) {
    UINT rc = on_reset(c, r);
    fprintf(stderr, "Wire reset %ux%u rc=%u\n", r->width, r->height, rc);
    portrait_reset = (rc == 0 && r->width == 864 && r->height == 1024);
    return rc;
}
static UINT end_frame(RdpgfxClientContext *c, const RDPGFX_END_FRAME_PDU *f) {
    UINT rc = on_frame(c, f);
    if (portrait_reset && rc == 0)
        portrait_frame = 1;
    return rc;
}
static void joined(void *ctx, const ChannelConnectedEventArgs *e) {
    fprintf(stderr, "CONTROL: channel %s\n", e->name);
    if (!strcmp(e->name, RDPGFX_DVC_CHANNEL_NAME)) {
        RdpgfxClientContext *gfx = e->pInterface;
        gdi_graphics_pipeline_init(((rdpContext *)ctx)->gdi, gfx);
        on_reset = gfx->ResetGraphics;
        gfx->ResetGraphics = reset;
        on_frame = gfx->EndFrame;
        gfx->EndFrame = end_frame;
    }
    if (!strcmp(e->name, DISP_DVC_CHANNEL_NAME)) {
        DispClientContext *d = e->pInterface;
        d->custom = ctx;
        d->DisplayControlCaps = caps;
    }
}
static BOOL pre(freerdp *f) {
    PubSub_SubscribeChannelConnected(f->context->pubSub, joined);
    return TRUE;
}
static int lastPainted;
static BOOL paint(rdpContext *c) {
    static int last_w, last_h;
    rdpGdi *g = c->gdi;
    if (g && g->primary_buffer && (last_w != g->width || last_h != g->height)) {
        unsigned char *buf = g->primary_buffer;
        for (int y = 0; y < g->height; y += 8)
            for (int x = 0; x < g->width; x += 8) {
                unsigned char *p = buf + y * g->stride + x * 4;
                if ((p[0] | p[1] | p[2]) != 0) {
                    fprintf(stderr, "PAINT %dx%d rgb=%u,%u,%u\n", g->width, g->height, p[2],
                            p[1], p[0]);
                    lastPainted = 1;
                    last_w = g->width;
                    last_h = g->height;
                    return TRUE;
                }
            }
    }
    return TRUE;
}
static BOOL resize(rdpContext *c) {
    return gdi_resize(c->gdi, freerdp_settings_get_uint32(c->settings, FreeRDP_DesktopWidth),
                      freerdp_settings_get_uint32(c->settings, FreeRDP_DesktopHeight));
}
static BOOL post(freerdp *f) {
    if (!gdi_init(f, PIXEL_FORMAT_BGRA32))
        return FALSE;
    f->context->update->DesktopResize = resize;
    f->context->update->EndPaint = paint;
    return TRUE;
}
int main(int argc, char **argv) {
    freerdp_register_addin_provider(freerdp_channels_load_static_addin_entry, 0);
    freerdp *f = freerdp_new();
    f->ContextSize = sizeof(rdpClientContext);
    if (!freerdp_context_new(f))
        return 2;
    rdpSettings *s = f->context->settings;
    freerdp_settings_set_string(s, FreeRDP_ServerHostname, "127.0.0.1");
    freerdp_settings_set_string(s, FreeRDP_Username, "resize-test");
    freerdp_settings_set_uint32(s, FreeRDP_ServerPort, 43989);
    freerdp_settings_set_bool(s, FreeRDP_IgnoreCertificate, TRUE);
    freerdp_settings_set_bool(s, FreeRDP_NlaSecurity, FALSE);
    freerdp_settings_set_bool(s, FreeRDP_SupportGraphicsPipeline, TRUE);
    freerdp_settings_set_bool(s, FreeRDP_SupportDisplayControl, TRUE);
    freerdp_settings_set_bool(s, FreeRDP_DynamicResolutionUpdate, TRUE);
    freerdp_settings_set_uint32(s, FreeRDP_DesktopWidth, 1024);
    freerdp_settings_set_uint32(s, FreeRDP_DesktopHeight, 768);
    freerdp_settings_set_bool(s, FreeRDP_GfxH264, TRUE);
    f->LoadChannels = freerdp_client_load_channels;
    const char *disp_name = "disp";
    const char *gfx_name = "rdpgfx";
    freerdp_client_add_dynamic_channel(s, 1, &disp_name);
    freerdp_client_add_dynamic_channel(s, 1, &gfx_name);
    freerdp_settings_set_bool(s, FreeRDP_SupportDynamicChannels, TRUE);
    f->PreConnect = pre;
    f->PostConnect = post;
    if (!freerdp_connect(f))
        return 3;
    fprintf(stderr, "CONNECTED\n");
    for (int i = 0; i < 16000; i++) {
        static int clicked;
        if (!clicked && lastPainted && display) {
            freerdp_input_send_mouse_event(f->context->input,
                                           PTR_FLAGS_BUTTON1 | PTR_FLAGS_DOWN, 20, 20);
            freerdp_input_send_mouse_event(f->context->input, PTR_FLAGS_BUTTON1, 20, 20);
            clicked = 1;
            fprintf(stderr, "clicked\n");
        }
        static int ticks;
        if (clicked && ++ticks == 60) {
            DISPLAY_CONTROL_MONITOR_LAYOUT portrait = {.Flags = 1,
                                                       .Width = 864,
                                                       .Height = 1024,
                                                       .DesktopScaleFactor = 100,
                                                       .DeviceScaleFactor = 100};
            if (display->SendMonitorLayout(display, 1, &portrait) != 0)
                return 4;
            fprintf(stderr, "sent portrait layout\n");
        }
        if (ticks > 60 && portrait_frame)
            return 0;
        if (i == 50)
            freerdp_input_send_mouse_event(f->context->input, PTR_FLAGS_MOVE, 20, 20);
        usleep(300);
        HANDLE h[64];
        DWORD n = freerdp_get_event_handles(f->context, h, 64);
        if (!n)
            break;
        WaitForMultipleObjects(n, h, FALSE, 20);
        if (!freerdp_check_event_handles(f->context)) {
            fprintf(stderr, "PEER CLOSED\n");
            break;
        }
    }
    return 5;
}
