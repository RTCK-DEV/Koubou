#ifndef KOUBOU_H
#define KOUBOU_H

/* session + dispatch live in koubou-composer; the rest in koubou-core */

#include <stdint.h>

typedef struct {
    uint8_t* data;
    uintptr_t len;
    uint32_t width;
    uint32_t height;
} KouImage;

/// 256-bin x 4-channel (R,G,B,luma) histogram of a rendered image.
typedef struct {
    uint32_t bins[1024];
} KouHistogram;



void* koubou_init(void);
void koubou_free_engine(void* e);
char* koubou_last_error(void);
void koubou_free_string(char* s);
void koubou_free_image(KouImage img);

char* koubou_scan_folder(void* e, const char* folder);
KouImage koubou_thumbnail(void* e, const char* path, uint32_t max_px);
KouImage koubou_render(void* e, const char* path, const char* recipe_json, uint32_t max_px);
KouImage koubou_render_h(void* e, const char* path, const char* recipe_json, uint32_t max_px, KouHistogram* hist);
KouImage koubou_scopes(void* e, const char* path, const char* recipe_json, uint32_t max_px, uint32_t* wave, uint32_t* vec, uint32_t* cie, uint32_t* hist);
KouImage koubou_export(void* e, const char* path, const char* recipe_json);
char* koubou_metadata(void* e, const char* path);
char* koubou_sidecar_read(const char* path);
int koubou_sidecar_write(const char* path, const char* json);
int koubou_set_rating(void* e, const char* path, int rating);
int koubou_set_label(void* e, const char* path, const char* label);
KouImage koubou_reference(const char* path);

char* koubou_auto_analyze(void* e, const char* path);

/* document session (koubou-composer): JSON command dispatch */
void* kou_session_new(void);
void kou_session_free(void* s);
char* kou_dispatch(void* s, const char* json);

#endif
