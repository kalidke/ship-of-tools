/* native_birth.c -- the two-phase launcher behind sot-log's host::process_tree (see native_birth.h).
 *
 * What runs in the forked child before the target's execve, and nothing else: close, sigaction, sigprocmask,
 * setsid, setpgid, open, dup2, fchdir, fcntl, getpid, getpgrp, getsid, write, read, execve and _exit, over the
 * launch description the parent built before the fork and fixed-size stack records. There is no allocation, no
 * environment access, no logging, no call back into Rust and no return from the child branch.
 */
#define _GNU_SOURCE
#include "native_birth.h"

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <string.h>
#include <unistd.h>
#ifdef __linux__
#include <sys/syscall.h>
#endif

#ifdef __linux__
#define SOT_SIGMAX _NSIG
#else
#define SOT_SIGMAX NSIG
#endif

#define GATE_GO 'G'
#define GATE_CANCEL 'C'

static void write_record(int fd, uint32_t stage, int err) {
    sot_record rec;
    memset(&rec, 0, sizeof rec);
    rec.stage = stage;
    rec.err = err;
    rec.pid = (int32_t)getpid();
    rec.pgid = (int32_t)getpgrp();
    rec.sid = (int32_t)getsid(0);
    ssize_t n;
    do {
        n = write(fd, &rec, sizeof rec);
    } while (n < 0 && errno == EINTR);
}

/* The child's one way out before the exec: say where and why on the ready pipe, then leave. */
/* Put one signal back to its default disposition. glibc refuses sigaction on the two signals its threads use
 * (32 and 33) with EINVAL, but a process can have inherited either ignored, and an ignored disposition survives the
 * exec; the kernel call has no such refusal. */
static void reset_disposition(int sig) {
    struct sigaction dfl;
    memset(&dfl, 0, sizeof dfl);
    dfl.sa_handler = SIG_DFL;
    sigemptyset(&dfl.sa_mask);
    if (sigaction(sig, &dfl, NULL) == 0) {
        return;
    }
#ifdef __linux__
    struct {
        void *handler;
        unsigned long flags;
        void *restorer;
        unsigned long mask;
    } kernel_default;
    memset(&kernel_default, 0, sizeof kernel_default);
    syscall(SYS_rt_sigaction, sig, &kernel_default, NULL, sizeof(unsigned long));
#endif
}

static void child_fail(int ready_w, uint32_t stage, int err) {
    write_record(ready_w, stage, err);
    _exit(126);
}

#ifdef SOT_BIRTH_FAULTS
/* A test barrier: tell the harness the child is at `stage`, then wait for its go. */
static void child_pause(const sot_launch *l, int stage) {
    if (l->pause_stage != stage || l->pause_out_fd < 0 || l->pause_in_fd < 0) {
        return;
    }
    char b = (char)stage;
    while (write(l->pause_out_fd, &b, 1) < 0 && errno == EINTR) {
    }
    ssize_t n;
    do {
        n = read(l->pause_in_fd, &b, 1);
    } while (n < 0 && errno == EINTR);
    if (n != 1) {
        _exit(125);
    }
}
#else
#define child_pause(l, stage) ((void)0)
#endif

static void child_branch(const sot_launch *l, int gate_r, int ready_w, int err_w) __attribute__((noreturn));

static void child_branch(const sot_launch *l, int gate_r, int ready_w, int err_w) {
    child_pause(l, SOT_PAUSE_BEFORE_CLOSE);
    for (size_t i = 0; i < l->n_close; i++) {
        close(l->close_fds[i]);
    }

    /* Every catchable signal is blocked since before the fork, so no inherited handler can run here. */
    for (int sig = 1; sig < SOT_SIGMAX; sig++) {
        if (sig != SIGKILL && sig != SIGSTOP) {
            reset_disposition(sig);
        }
    }

    child_pause(l, SOT_PAUSE_BEFORE_SESSION);
    if (l->new_session && setsid() < 0) {
        child_fail(ready_w, SOT_STAGE_SESSION, errno);
    }
    if (l->join_pgid != 0 && setpgid(0, l->join_pgid > 0 ? l->join_pgid : 0) < 0) {
        child_fail(ready_w, SOT_STAGE_GROUP, errno);
    }

    for (int i = 0; i < 3; i++) {
        int src = l->stdio_fds[i];
        if (src < 0) {
            src = open("/dev/null", O_RDWR);
            if (src < 0) {
                child_fail(ready_w, SOT_STAGE_STDIO, errno);
            }
        }
        if (src != i) {
            if (dup2(src, i) < 0) {
                child_fail(ready_w, SOT_STAGE_STDIO, errno);
            }
        } else if (fcntl(i, F_SETFD, 0) < 0) {
            child_fail(ready_w, SOT_STAGE_STDIO, errno);
        }
    }
    if (l->cwd_fd >= 0 && fchdir(l->cwd_fd) < 0) {
        child_fail(ready_w, SOT_STAGE_CWD, errno);
    }
    for (size_t i = 0; i < l->n_inherit; i++) {
        if (fcntl(l->inherit_fds[i], F_SETFD, 0) < 0) {
            child_fail(ready_w, SOT_STAGE_INHERIT, errno);
        }
    }

    child_pause(l, SOT_PAUSE_BEFORE_READY);
    write_record(ready_w, SOT_STAGE_READY, 0);

    /* The gate: only an explicit GO runs the target. */
    char b = 0;
    ssize_t n;
    do {
        n = read(gate_r, &b, 1);
    } while (n < 0 && errno == EINTR);
    if (n != 1 || b != GATE_GO) {
        _exit(125);
    }

    sigset_t none;
    sigemptyset(&none);
    sigprocmask(SIG_SETMASK, &none, NULL);
    execve(l->path, l->argv, l->envp);
    write_record(err_w, SOT_STAGE_EXEC, errno);
    _exit(127);
}

static int pipe_cloexec(int fds[2]) {
#ifdef __linux__
    return pipe2(fds, O_CLOEXEC);
#else
    /* The caller serializes births (the adapter's birth transaction), so no other thread forks between these. */
    if (pipe(fds) < 0) {
        return -1;
    }
    for (int i = 0; i < 2; i++) {
        if (fcntl(fds[i], F_SETFD, FD_CLOEXEC) < 0) {
            int e = errno;
            close(fds[0]);
            close(fds[1]);
            errno = e;
            return -1;
        }
    }
    return 0;
#endif
}

static void close_all(const int *fds, int n) {
    for (int i = 0; i < n; i++) {
        if (fds[i] >= 0) {
            close(fds[i]);
        }
    }
}

int sot_birth_begin(const sot_launch *launch, sot_birth *out) {
    if (launch == NULL || out == NULL || launch->abi != SOT_BIRTH_ABI || launch->path == NULL || launch->argv == NULL ||
        launch->envp == NULL) {
        return EINVAL;
    }
    int gate[2], ready[2], error[2];
    if (pipe_cloexec(gate) < 0) {
        return errno;
    }
    if (pipe_cloexec(ready) < 0) {
        int e = errno;
        close_all(gate, 2);
        return e;
    }
    if (pipe_cloexec(error) < 0) {
        int e = errno;
        close_all(gate, 2);
        close_all(ready, 2);
        return e;
    }

    sigset_t all, old;
    sigfillset(&all);
    pthread_sigmask(SIG_SETMASK, &all, &old);
    pid_t pid = fork();
    if (pid < 0) {
        int e = errno;
        pthread_sigmask(SIG_SETMASK, &old, NULL);
        close_all(gate, 2);
        close_all(ready, 2);
        close_all(error, 2);
        return e;
    }
    if (pid == 0) {
        /* The parent-only ends go first: the child must never hold a writer that keeps the parent's reads open. */
        close(gate[1]);
        close(ready[0]);
        close(error[0]);
        child_branch(launch, gate[0], ready[1], error[1]);
    }

    pthread_sigmask(SIG_SETMASK, &old, NULL);
    close(gate[0]);
    close(ready[1]);
    close(error[1]);
    out->pid = pid;
    out->gate_fd = gate[1];
    out->ready_fd = ready[0];
    out->error_fd = error[0];
    return 0;
}

static int spend_gate(sot_birth *birth, char byte) {
    if (birth == NULL || birth->gate_fd < 0) {
        return EBADF;
    }
    int err = 0;
    ssize_t n;
    do {
        n = write(birth->gate_fd, &byte, 1);
    } while (n < 0 && errno == EINTR);
    if (n != 1) {
        err = n < 0 ? errno : EIO;
    }
    close(birth->gate_fd);
    birth->gate_fd = -1;
    return err;
}

int sot_birth_release(sot_birth *birth) {
    return spend_gate(birth, GATE_GO);
}

int sot_birth_cancel(sot_birth *birth) {
    return spend_gate(birth, GATE_CANCEL);
}
