#include "kou_shim.h"

#include <libraw/libraw.h>

#include <cstdlib>
#include <cstring>

struct KouRaw {
  LibRaw* raw;
};

extern "C" {

KouRaw* kou_raw_open(const char* path, KouRawInfo* info) {
  if (!path || !info) return nullptr;
  LibRaw* r = new LibRaw();
  if (r->open_file(path) != LIBRAW_SUCCESS) {
    delete r;
    return nullptr;
  }
  r->imgdata.params.use_camera_wb = 1;
  r->imgdata.params.output_color = 0;      /* raw camera space */
  r->imgdata.params.gamm[0] = 1.0f;
  r->imgdata.params.gamm[1] = 1.0f;        /* linear gamma */
  r->imgdata.params.no_auto_bright = 1;
  r->imgdata.params.use_camera_matrix = 1;
  r->imgdata.params.user_qual = 0;         /* linear demosaic for reference path */

  memset(info, 0, sizeof(*info));
  libraw_data_t& id = r->imgdata;
  strncpy(info->make, id.idata.make, sizeof(info->make) - 1);
  strncpy(info->model, id.idata.model, sizeof(info->model) - 1);
  strncpy(info->lens, id.lens.Lens, sizeof(info->lens) - 1);
  info->raw_width = id.sizes.raw_width;
  info->raw_height = id.sizes.raw_height;
  info->width = id.sizes.width;
  info->height = id.sizes.height;
  info->top_margin = id.sizes.top_margin;
  info->left_margin = id.sizes.left_margin;
  info->flip = id.sizes.flip;
  info->maximum = id.color.maximum ? id.color.maximum : 65535u;
  for (int i = 0; i < 4; i++) {
    info->black[i] = (float)(id.color.black + id.color.cblack[i]);
    info->cam_mul[i] = id.color.cam_mul[i];
  }
  memcpy(info->cam_xyz, id.color.cam_xyz, sizeof(info->cam_xyz));
  for (int i = 0; i < 4; i++) info->pre_mul[i] = id.color.pre_mul[i];
  memcpy(info->rgb_cam, id.color.rgb_cam, sizeof(info->rgb_cam));
  info->daylight_mul_valid = 0;
  info->colors = id.idata.colors;

  if (id.idata.filters == 9) { /* Fuji X-Trans */
    info->cfa_kind = 2;
    info->cfa_w = 6;
    info->cfa_h = 6;
    for (int rr = 0; rr < 6; rr++)
      for (int cc = 0; cc < 6; cc++)
        info->cfa_pattern[rr * 6 + cc] = id.idata.xtrans[rr][cc];
  } else if (id.idata.filters && id.idata.filters != 1) {
    info->cfa_kind = 1;
    info->cfa_w = 2;
    info->cfa_h = 2;
    for (int rr = 0; rr < 2; rr++)
      for (int cc = 0; cc < 2; cc++)
        info->cfa_pattern[rr * 2 + cc] =
            (id.idata.filters >> (((rr << 1 & 14) + (cc & 1)) << 1)) & 3;
  } else {
    info->cfa_kind = 0;
  }

  info->iso = id.other.iso_speed;
  info->shutter = id.other.shutter;
  info->aperture = id.other.aperture;
  info->focal = id.other.focal_len;
  info->timestamp = (long)id.other.timestamp;

  return new KouRaw{r};
}

int kou_raw_unpack(KouRaw* r) {
  if (!r) return -1;
  return r->raw->unpack();
}

void kou_raw_refresh_info(KouRaw* r, KouRawInfo* info) {
  if (!r || !info) return;
  libraw_data_t& id = r->raw->imgdata;
  info->maximum = id.color.maximum ? id.color.maximum : 65535u;
  for (int i = 0; i < 4; i++) {
    info->black[i] = (float)(id.color.black + id.color.cblack[i]);
    info->cam_mul[i] = id.color.cam_mul[i];
    info->pre_mul[i] = id.color.pre_mul[i];
  }
  memcpy(info->rgb_cam, id.color.rgb_cam, sizeof(info->rgb_cam));
}

int kou_raw_cfa(KouRaw* r, unsigned short** out, int* count) {
  if (!r || !out || !count) return -1;
  libraw_data_t& id = r->raw->imgdata;
  if (!id.rawdata.raw_image) return -2;
  int n = id.sizes.raw_width * id.sizes.raw_height;
  unsigned short* buf = (unsigned short*)malloc((size_t)n * sizeof(unsigned short));
  if (!buf) return -3;
  memcpy(buf, id.rawdata.raw_image, (size_t)n * sizeof(unsigned short));
  *out = buf;
  *count = n;
  return 0;
}

int kou_thumb(KouRaw* r, unsigned char** out, int* len, int* w, int* h,
              int* format) {
  if (!r || !out || !len) return -1;
  if (r->raw->unpack_thumb() != LIBRAW_SUCCESS) return -2;
  libraw_thumbnail_t& t = r->raw->imgdata.thumbnail;
  if (!t.thumb || t.tlength <= 0) return -3;
  unsigned char* buf = (unsigned char*)malloc((size_t)t.tlength);
  if (!buf) return -4;
  memcpy(buf, t.thumb, (size_t)t.tlength);
  *out = buf;
  *len = t.tlength;
  if (w) *w = t.twidth;
  if (h) *h = t.theight;
  if (format) *format = (int)t.tformat;
  return 0;
}

int kou_process8(KouRaw* r, unsigned char** out, int* w, int* h) {
  if (!r || !out) return -1;
  /* reference render uses libraw's own pipeline: sRGB + gamma */
  r->raw->imgdata.params.output_color = 1;
  r->raw->imgdata.params.gamm[0] = 2.222f;
  r->raw->imgdata.params.gamm[1] = 4.5f;
  r->raw->imgdata.params.no_auto_bright = 0;
  int urc = r->raw->unpack();
  if (urc != LIBRAW_SUCCESS) return 10000 + (-urc);
  int prc = r->raw->dcraw_process();
  if (prc != LIBRAW_SUCCESS) return 20000 + (-prc);
  int err = 0;
  libraw_processed_image_t* img = r->raw->dcraw_make_mem_image(&err);
  if (!img || img->type != LIBRAW_IMAGE_BITMAP || img->colors != 3 ||
      img->bits != 8) {
    return -3;
  }
  unsigned char* buf = (unsigned char*)malloc(img->data_size);
  if (!buf) {
    r->raw->dcraw_clear_mem(img);
    return -4;
  }
  memcpy(buf, img->data, img->data_size);
  *out = buf;
  if (w) *w = img->width;
  if (h) *h = img->height;
  r->raw->dcraw_clear_mem(img);
  return 0;
}

void kou_raw_close(KouRaw* r) {
  if (!r) return;
  if (r->raw) {
    delete r->raw;
  }
  delete r;
}

void kou_free(void* p) { free(p); }

} /* extern "C" */
