#define _GNU_SOURCE

#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

extern char **environ;

static void write_error(const char *message) {
  (void)write(STDERR_FILENO, "Hooviestar AppRun bootstrap: ", 29);
  (void)write(STDERR_FILENO, message, strlen(message));
  (void)write(STDERR_FILENO, "\n", 1);
}

int main(int argc, char **argv) {
  char app_dir[PATH_MAX];
  const ssize_t length = readlink("/proc/self/exe", app_dir, sizeof(app_dir) - 1);
  if (length <= 0 || (size_t)length >= sizeof(app_dir) - 1) {
    write_error("cannot resolve /proc/self/exe");
    return 127;
  }
  app_dir[length] = '\0';

  char *separator = strrchr(app_dir, '/');
  if (separator == NULL) {
    write_error("resolved executable has no parent directory");
    return 127;
  }
  if (separator == app_dir) {
    app_dir[1] = '\0';
  } else {
    *separator = '\0';
  }

  char app_run_shell[PATH_MAX];
  const int shell_length = snprintf(app_run_shell, sizeof(app_run_shell), "%s/AppRun.shell", app_dir);
  if (shell_length < 0 || (size_t)shell_length >= sizeof(app_run_shell)) {
    write_error("AppRun.shell path is too long");
    return 127;
  }

  if (setenv("APPDIR", app_dir, 1) != 0) {
    write_error("cannot set APPDIR");
    return 127;
  }
  if (unsetenv("LD_LIBRARY_PATH") != 0) {
    write_error("cannot clear inherited LD_LIBRARY_PATH");
    return 127;
  }

  char *child_argv[argc + 1];
  child_argv[0] = app_run_shell;
  for (int index = 1; index < argc; index++) {
    child_argv[index] = argv[index];
  }
  child_argv[argc] = NULL;

  execve(app_run_shell, child_argv, environ);
  char error_message[PATH_MAX + 64];
  const int error_length = snprintf(
      error_message,
      sizeof(error_message),
      "cannot exec %s: %s",
      app_run_shell,
      strerror(errno));
  if (error_length > 0) {
    error_message[sizeof(error_message) - 1] = '\0';
    write_error(error_message);
  }
  return 127;
}
