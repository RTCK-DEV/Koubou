#ifndef KOU_SHIM_H
#define KOU_SHIM_H

/* SPDX-License-Identifier: MIT */

#ifdef __cplusplus
extern "C" {
#endif

typedef struct KouRaw KouRaw;

typedef struct KouRawInfo {
  char make[64];
  char model[64];
  char lens[128];
  int raw_width;
  int raw_height;
  int width;
  int height;
  int top_margin;
  int left_margin;
  int flip;
  float black[4];
  unsigned int maximum;
  float cam_mul[4];
  float pre_mul[4];
  float cam_xyz[4][3];
  float rgb_cam[3][4];
  int cfa_kind;      /* 0 = already RGB / unknown, 1 = bayer-like via filters, 2 = xtrans */
  int cfa_pattern[36];
  int cfa_w;
  int cfa_h;
  int colors;        /* number of color channels in CFA (3 or 4) */
  int daylight_mul_valid;
  float iso;
  float shutter;
  float aperture;
  float focal;
  long timestamp;
} KouRawInfo;

KouRaw* kou_raw_open(const char* path, KouRawInfo* info);
int kou_raw_unpack(KouRaw* r);
/* re-read fields that libraw computes during unpack (black level,
   maximum, pre_mul, rgb_cam). Call after kou_raw_unpack(). */
void kou_raw_refresh_info(KouRaw* r, KouRawInfo* info);
/* returns malloc'ed copy of raw sensor data (cfa mosaic); free with kou_free */
int kou_raw_cfa(KouRaw* r, unsigned short** out, int* count);
/* returns malloc'ed thumbnail bytes; format: 1=jpeg,2=bitmap8,3=bitmap16 */
int kou_thumb(KouRaw* r, unsigned char** out, int* len, int* w, int* h, int* format);
/* renders with libraw's own pipeline -> malloc'ed rgb8 buffer (reference path) */
int kou_process8(KouRaw* r, unsigned char** out, int* w, int* h);
void kou_raw_close(KouRaw* r);
void kou_free(void* p);

#ifdef __cplusplus
}
#endif

#endif
