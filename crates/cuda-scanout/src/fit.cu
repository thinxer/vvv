// Build: nvcc -ptx -arch=compute_75 fit.cu -o fit.ptx
extern "C" __global__ void fit(const uchar4 *src, int stride, int sw, int sh,
                               uchar4 *dst, int pitch, int w, int h,
                               int left, int top, int fw, int fh,
                               const uchar4 *cursor, int cx, int cy, int cw, int ch) {
    int x = blockIdx.x * blockDim.x + threadIdx.x;
    int y = blockIdx.y * blockDim.y + threadIdx.y;
    if (x >= w || y >= h) return;
    uchar4 color = make_uchar4(16, 19, 24, 255);
    if (x >= left && x < left + fw && y >= top && y < top + fh) {
        float sx = fmaxf(0, (x-left+.5f)*sw/fw-.5f);
        float sy = fmaxf(0, (y-top+.5f)*sh/fh-.5f);
        int ix = int(sx), iy = int(sy);
        uchar4 a = src[iy*stride+ix], b = src[iy*stride+min(ix+1,sw-1)];
        uchar4 c = src[min(iy+1,sh-1)*stride+ix], d = src[min(iy+1,sh-1)*stride+min(ix+1,sw-1)];
        float u = sx-ix, v = sy-iy;
        for (int k=0;k<3;k++) {
            float p = ((unsigned char*)&a)[k]*(1-u) + ((unsigned char*)&b)[k]*u;
            float q = ((unsigned char*)&c)[k]*(1-u) + ((unsigned char*)&d)[k]*u;
            ((unsigned char*)&color)[k] = __float2uint_rn(p*(1-v)+q*v);
        }
        if (cursor) {
            int i = (x-left)*sw/fw - cx, j = (y-top)*sh/fh - cy;
            if (unsigned(i) < unsigned(cw) && unsigned(j) < unsigned(ch)) {
                uchar4 fg = cursor[j*cw+i]; // Host ARGB words are BGRA in memory.
                unsigned a = fg.w;
                color = make_uchar4((fg.z*a+color.x*(255-a)+127)/255,
                                    (fg.y*a+color.y*(255-a)+127)/255,
                                    (fg.x*a+color.z*(255-a)+127)/255, 255);
            }
        }
    }
    dst[y*pitch+x] = color;
}

// dest contains the uploaded label; underlay contains the saved guest pixels.
extern "C" __global__ void blend_label(uchar4* dest, int stride, const uchar4* underlay,
                                       int width, int height) {
    int x = blockIdx.x * blockDim.x + threadIdx.x;
    int y = blockIdx.y * blockDim.y + threadIdx.y;
    if (x >= width || y >= height) return;
    uchar4 fg = dest[y*stride+x], bg = underlay[y*width+x];
    unsigned a = fg.w;
    dest[y*stride+x] = make_uchar4((fg.x*a+bg.x*(255-a)+127)/255,
                                   (fg.y*a+bg.y*(255-a)+127)/255,
                                   (fg.z*a+bg.z*(255-a)+127)/255, 255);
}
