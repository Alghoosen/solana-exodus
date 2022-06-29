#pragma once
/**
 * @brief Solana string and memory system calls and utilities
 */

#include <sol/types.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * Copies memory
 * dst and src may not overlap
 */
static void sol_memcpy(void *dst, const void *src, int len) {
  if (dst == (void *)0 || src == (void *)0)
    return ;

  for (int i = 0; i < len; i++) {
    *((uint8_t *)dst + i) = *((const uint8_t *)src + i);
  }
}

/**
 * Copies memory
 * dst and src may overlap
 */
static void sol_memmove(void *dst, const void *src, int len) {
    char *dest = dst;
    const char *source = src;

    if (!dest || !source || source == dest)
        return ;
    // If dst is before source, use memcpy since it copies forward
    if (dest < source)
        sol_memcpy(dst, src, len);
    // copy from the back
    for (int i = len - 1; i >= 0; i--)
        dest[i] = source[i];
}


/**
 * Compares memory
 */
static int sol_memcmp(const void *s1, const void *s2, int n) {
  if (s1 == (void *)0 || s2 == (void *)0)
    return 1;

  for (int i = 0; i < n; i++) {
    uint8_t diff = *((uint8_t *)s1 + i) - *((const uint8_t *)s2 + i);
    if (diff) {
      return diff;
    }
  }
  return 0;
}

/**
 * Fill a byte string with a byte value
 */
static void *sol_memset(void *b, int c, size_t len) {
  uint8_t *a = (uint8_t *) b;

  if (!b)
    return ;

  while (len > 0) {
    *a = c;
    a++;
    len--;
  }
}

/**
 * Find length of string
 * Checks 4 chars at a time for faster performance
 */
static size_t sol_strlen(const char *s) {
  const char *ref = s;

  if (!s)
    return 0;

  while (1)
  {
    if (s[0] == '\0')
      return s - ref + 0;
    if (s[1] == '\0')
      return s - ref + 1;
    if (s[2] == '\0')
      return s - ref + 2;
    if (s[3] == '\0')
      return s - ref + 3;
    s += 4;
  }
}

/**
 * Start address of the memory region used for program heap.
 */
#define HEAP_START_ADDRESS (0x300000000)
/**
 * Length of the heap memory region used for program heap.
 */
#define HEAP_LENGTH (32 * 1024)

/**
 * Alloc zero-initialized memory
 */
static void *sol_calloc(size_t nitems, size_t size) {
  // Bump allocator
  uint64_t* pos_ptr = (uint64_t*)HEAP_START_ADDRESS;

  uint64_t pos = *pos_ptr;
  if (pos == 0) {
      /** First time, set starting position */
      pos = HEAP_START_ADDRESS + HEAP_LENGTH;
  }

  uint64_t bytes = (uint64_t)(nitems * size);
  if (size == 0 ||
      !(nitems == 0 || size == 0) &&
      !(nitems == bytes / size)) {
    /** Overflow */
    return NULL;
  }
  if (pos < bytes) {
    /** Saturated */
    pos = 0;
  } else {
    pos -= bytes;
  }

  uint64_t align = size;
  align--;
  align |= align >> 1;
  align |= align >> 2;
  align |= align >> 4;
  align |= align >> 8;
  align |= align >> 16;
  align |= align >> 32;
  align++;
  pos &= ~(align - 1);
  if (pos < HEAP_START_ADDRESS + sizeof(uint8_t*)) {
      return NULL;
  }
  *pos_ptr = pos;
  return (void*)pos;
}

/**
 * Deallocates the memory previously allocated by sol_calloc
 */
static void sol_free(void *ptr) {
  // I'm a bump allocator, I don't free
}

#ifdef __cplusplus
}
#endif

/**@}*/
