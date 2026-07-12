#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

static void fail(const char *message) {
  fprintf(stderr, "secure-snapshot: %s\n", message);
  exit(EXIT_FAILURE);
}

static void close_checked(int fd) {
  while (close(fd) < 0) {
    if (errno != EINTR) {
      fail("close failed");
    }
  }
}

static int safe_component(const char *component) {
  return component[0] != '\0' && strcmp(component, ".") != 0 &&
         strcmp(component, "..") != 0 && strchr(component, '/') == NULL;
}

static int open_source(const char *root, const char *relative) {
  if (relative[0] == '\0' || relative[0] == '/' ||
      strlen(relative) >= PATH_MAX || relative[strlen(relative) - 1] == '/') {
    fail("invalid source path");
  }

  char *copy = strdup(relative);
  if (copy == NULL) {
    fail("out of memory");
  }

  int directory = open(root, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
  if (directory < 0) {
    free(copy);
    fail("source root is not a safe directory");
  }

  char *save = NULL;
  char *component = strtok_r(copy, "/", &save);
  if (component == NULL) {
    close_checked(directory);
    free(copy);
    fail("invalid source path");
  }

  for (;;) {
    if (!safe_component(component)) {
      close_checked(directory);
      free(copy);
      fail("invalid source path component");
    }

    char *next = strtok_r(NULL, "/", &save);
    if (next == NULL) {
      int source = openat(directory, component,
                          O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC);
      close_checked(directory);
      free(copy);
      if (source < 0) {
        fail("source file could not be opened safely");
      }
      return source;
    }

    int child = openat(directory, component,
                       O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    if (child < 0) {
      close_checked(directory);
      free(copy);
      fail("source directory could not be traversed safely");
    }
    close_checked(directory);
    directory = child;
    component = next;
  }
}

static uint64_t parse_limit(const char *value) {
  if (value[0] == '\0' || value[0] == '-') {
    fail("invalid size limit");
  }
  errno = 0;
  char *end = NULL;
  unsigned long long parsed = strtoull(value, &end, 10);
  if (errno != 0 || end == value || *end != '\0' || parsed == 0) {
    fail("invalid size limit");
  }
  return (uint64_t)parsed;
}

static int same_snapshot(const struct stat *before, const struct stat *after) {
  return before->st_dev == after->st_dev && before->st_ino == after->st_ino &&
         before->st_size == after->st_size &&
         before->st_mtim.tv_sec == after->st_mtim.tv_sec &&
         before->st_mtim.tv_nsec == after->st_mtim.tv_nsec &&
         before->st_ctim.tv_sec == after->st_ctim.tv_sec &&
         before->st_ctim.tv_nsec == after->st_ctim.tv_nsec;
}

int main(int argc, char **argv) {
  if (argc != 6) {
    fail("usage: secure-snapshot SOURCE_ROOT RELATIVE DEST_DIR DEST_NAME MAX_BYTES");
  }

  const char *source_root = argv[1];
  const char *relative = argv[2];
  const char *destination_root = argv[3];
  const char *destination_name = argv[4];
  const uint64_t maximum = parse_limit(argv[5]);

  if (!safe_component(destination_name)) {
    fail("invalid destination name");
  }

  int source = open_source(source_root, relative);
  struct stat before;
  if (fstat(source, &before) < 0 || !S_ISREG(before.st_mode) || before.st_size <= 0 ||
      (before.st_mode & (S_IWGRP | S_IWOTH)) != 0 ||
      (uint64_t)before.st_size > maximum) {
    close_checked(source);
    fail("source must be a non-empty, non-shared-writable regular file within the size limit");
  }

  int destination_directory =
      open(destination_root, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
  if (destination_directory < 0) {
    close_checked(source);
    fail("destination is not a safe directory");
  }

  int destination = openat(destination_directory, destination_name,
                           O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC,
                           S_IRUSR);
  if (destination < 0) {
    close_checked(destination_directory);
    close_checked(source);
    fail("destination file could not be created safely");
  }

  uint64_t total = 0;
  char buffer[32768];
  for (;;) {
    ssize_t count = read(source, buffer, sizeof(buffer));
    if (count < 0) {
      if (errno == EINTR) {
        continue;
      }
      unlinkat(destination_directory, destination_name, 0);
      close_checked(destination);
      close_checked(destination_directory);
      close_checked(source);
      fail("source read failed");
    }
    if (count == 0) {
      break;
    }
    total += (uint64_t)count;
    if (total > maximum) {
      unlinkat(destination_directory, destination_name, 0);
      close_checked(destination);
      close_checked(destination_directory);
      close_checked(source);
      fail("source grew beyond the size limit");
    }

    ssize_t offset = 0;
    while (offset < count) {
      ssize_t written = write(destination, buffer + offset, (size_t)(count - offset));
      if (written < 0) {
        if (errno == EINTR) {
          continue;
        }
        unlinkat(destination_directory, destination_name, 0);
        close_checked(destination);
        close_checked(destination_directory);
        close_checked(source);
        fail("destination write failed");
      }
      offset += written;
    }
  }

  struct stat after;
  if (fstat(source, &after) < 0 || !same_snapshot(&before, &after) ||
      total != (uint64_t)before.st_size) {
    unlinkat(destination_directory, destination_name, 0);
    close_checked(destination);
    close_checked(destination_directory);
    close_checked(source);
    fail("source changed while it was being copied");
  }

  if (fchmod(destination, S_IRUSR) < 0 || fdatasync(destination) < 0) {
    unlinkat(destination_directory, destination_name, 0);
    close_checked(destination);
    close_checked(destination_directory);
    close_checked(source);
    fail("destination could not be finalized");
  }

  close_checked(destination);
  close_checked(destination_directory);
  close_checked(source);
  return EXIT_SUCCESS;
}
