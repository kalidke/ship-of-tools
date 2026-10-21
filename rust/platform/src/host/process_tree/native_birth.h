/* native_birth.h -- the fixed C ABI of the two-phase process launcher (sot-log, platform).
 *
 * `sot_birth_begin` forks and returns to its caller at once with an OWNING result: the child's pid (its parent is
 * the caller, so only the caller can wait for it), the gate's write end, and the two status pipes. The child runs
 * only the native branch in native_birth.c, then blocks on the gate; a GO byte makes it exec the target, anything
 * else (a CANCEL byte, EOF, a malformed byte) makes it `_exit` before the target runs. No Rust runs in the child.
 */
#ifndef SOT_NATIVE_BIRTH_H
#define SOT_NATIVE_BIRTH_H

#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>

#define SOT_BIRTH_ABI 1

/* The stage a status record names, in the order the child passes them. */
enum sot_birth_stage {
    SOT_STAGE_CLOSE = 1,   /* closing the parent-only endpoints */
    SOT_STAGE_SIGNALS = 2, /* resetting dispositions */
    SOT_STAGE_SESSION = 3, /* setsid */
    SOT_STAGE_GROUP = 4,   /* setpgid */
    SOT_STAGE_STDIO = 5,   /* dup2 onto 0, 1, 2 */
    SOT_STAGE_CWD = 6,     /* fchdir */
    SOT_STAGE_INHERIT = 7, /* clearing FD_CLOEXEC on the descriptors that cross the exec */
    SOT_STAGE_READY = 8,   /* the child is set up and waits for the gate; a success record */
    SOT_STAGE_GATE = 9,    /* the gate said CANCEL, closed, or sent something malformed */
    SOT_STAGE_EXEC = 10    /* execve failed */
};

/* Where a test barrier may hold the child (only in a build with SOT_BIRTH_FAULTS; otherwise ignored). */
enum sot_birth_pause {
    SOT_PAUSE_NONE = 0,
    SOT_PAUSE_BEFORE_CLOSE = 1,
    SOT_PAUSE_BEFORE_SESSION = 2,
    SOT_PAUSE_BEFORE_READY = 3
};

typedef struct sot_launch {
    uint32_t abi;
    const char *path;          /* absolute path of the target */
    char *const *argv;         /* NULL-terminated */
    char *const *envp;         /* NULL-terminated */
    int cwd_fd;                /* an open directory to fchdir into, or -1 */
    int stdio_fds[3];          /* descriptors dup2'd onto 0, 1, 2; -1 leaves /dev/null there */
    const int *close_fds;      /* endpoints that belong to the parent only, closed first */
    size_t n_close;
    const int *inherit_fds;    /* descriptors that must stay open across the exec */
    size_t n_inherit;
    int new_session;           /* nonzero: setsid() */
    pid_t join_pgid;           /* >0: setpgid(0, join_pgid); -1: setpgid(0, 0); 0: leave the group */
    int pause_stage;           /* enum sot_birth_pause; test builds only */
    int pause_out_fd;          /* the child writes one byte (the stage) here ... */
    int pause_in_fd;           /* ... and then reads one byte here; EOF is _exit(125) */
} sot_launch;

/* What the child writes: one fixed-size record on the ready pipe (a success record has stage READY), one on
 * the error pipe if execve fails. */
typedef struct sot_record {
    uint32_t stage;
    int32_t err;
    int32_t pid;
    int32_t pgid;
    int32_t sid;
} sot_record;

typedef struct sot_birth {
    pid_t pid;
    int gate_fd;   /* parent write end of the one-use gate */
    int ready_fd;  /* parent read end: one sot_record */
    int error_fd;  /* parent read end: EOF means the exec happened; else one sot_record */
} sot_birth;

/* Fork the child. Returns 0 with `out` filled, or an errno with no child and no descriptor left open. */
int sot_birth_begin(const sot_launch *launch, sot_birth *out);

/* Write the GO / CANCEL byte on the gate and close it: the gate is spent either way. Return 0 or an errno. */
int sot_birth_release(sot_birth *birth);
int sot_birth_cancel(sot_birth *birth);

#endif
