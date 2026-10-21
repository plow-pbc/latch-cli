// Full Disk Access spike: does a signed latch-like binary, started by a
// launchd agent, get FDA attributed to itself? It opens two protected
// paths read-only, closes them at once, reads nothing, and prints
// granted / not_granted / unavailable for each. With --dry it opens
// nothing and only says who it is, to check the launchd plumbing.
//
// Variant C's chain: `--chain OUTSIDE` (the app bundle's executable) starts
// the separate binary OUTSIDE with `--worker`, which starts itself again
// with no arguments as its own child: that grandchild probes. Each waits
// for its child. Add --dry after either flag to check the chain alone.
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <mach-o/dyld.h>
#include <pwd.h>
#include <spawn.h>
#include <sys/wait.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#ifndef BUILD
#define BUILD "unknown"
#endif

static void probe(const char *name, const char *home, const char *rel, int flags) {
    char path[PATH_MAX];
    snprintf(path, sizeof path, "%s/%s", home, rel);
    int fd = open(path, flags);
    if (fd >= 0) {
        close(fd);
        printf("%s: granted\n", name);
    } else if (errno == EPERM || errno == EACCES) {
        printf("%s: not_granted errno=%d (%s)\n", name, errno, strerror(errno));
    } else {
        printf("%s: unavailable errno=%d (%s)\n", name, errno, strerror(errno));
    }
}

extern char **environ;

/// Starts `argv[0]` as a child and waits for it; its exit status, or -1.
static int run_child(char *const argv[]) {
    pid_t pid;
    int err = posix_spawn(&pid, argv[0], NULL, NULL, argv, environ);
    if (err != 0) {
        printf("spawn %s failed: %s\n", argv[0], strerror(err));
        return -1;
    }
    fflush(stdout);
    int status = 0;
    while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {
    }
    return status;
}

int main(int argc, char **argv) {
    char exe[PATH_MAX];
    uint32_t size = sizeof exe;
    if (_NSGetExecutablePath(exe, &size) != 0) {
        snprintf(exe, sizeof exe, "?");
    }
    struct passwd *pw = getpwuid(getuid());
    const char *home = pw ? pw->pw_dir : "/nonexistent";
    const char *role = argc > 1 && strcmp(argv[1], "--chain") == 0    ? "chain"
                       : argc > 1 && strcmp(argv[1], "--worker") == 0 ? "outside"
                                                                       : "probe";
    printf("role=%s build=%s pid=%d ppid=%d uid=%d exe=%s\n", role, BUILD, getpid(), getppid(),
           getuid(), exe);
    fflush(stdout);
    int dry = argc > 1 && strcmp(argv[argc - 1], "--dry") == 0;
    if (strcmp(role, "chain") == 0 && argc > 2) {
        char *child[] = {argv[2], "--worker", dry ? "--dry" : NULL, NULL};
        run_child(child);
        return 0;
    }
    if (strcmp(role, "outside") == 0) {
        char *child[] = {exe, dry ? "--dry" : NULL, NULL};
        run_child(child);
        return 0;
    }
    if (dry) {
        printf("dry: no protected path opened\n");
        return 0;
    }
    probe("messages_chat_db", home, "Library/Messages/chat.db", O_RDONLY);
    probe("addressbook_dir", home, "Library/Application Support/AddressBook", O_RDONLY | O_DIRECTORY);
    return 0;
}
