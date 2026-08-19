2026-08-19T19:55:45.625462Z 00O [0KRunning with gitlab-runner 19.3.0~pre.1819.g9cbf0074 (9cbf0074)[0;m
2026-08-19T19:55:45.625500Z 00O [0K  on lunarwing-lint-docker lYlWumyKW, system ID: r_dLo2MDLTpMm2[0;m
2026-08-19T19:55:45.625575Z 00O section_start:1787169345:prepare_executor
[0K
2026-08-19T19:55:45.625584Z 00O+[0K[36;1mPreparing the "docker" executor[0;m[0;m
2026-08-19T19:55:45.651952Z 00O [0KUsing Docker executor with image docker.io/library/python:3.12-slim-bookworm@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b ...[0;m
2026-08-19T19:55:46.761161Z 00O [0KUsing effective pull policy of [always] for container docker.io/library/python:3.12-slim-bookworm@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b[0;m
2026-08-19T19:55:46.762940Z 00O [0KPulling docker image docker.io/library/python:3.12-slim-bookworm@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b ...[0;m
2026-08-19T19:55:47.028076Z 00O [0KUsing docker image sha256:a1e028236d55ab8a2ec45d7c693137b5adb5d230baecdbae4f9f75cf51b6ce66 for docker.io/library/python:3.12-slim-bookworm@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b with digest python@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b ...[0;m
2026-08-19T19:55:47.028092Z 00O section_end:1787169347:prepare_executor
[0K
2026-08-19T19:55:47.028210Z 00O+section_start:1787169347:prepare_script
[0K
2026-08-19T19:55:47.028214Z 00O+[0K[36;1mPreparing environment[0;m[0;m
2026-08-19T19:55:47.029475Z 00O [0KUsing effective pull policy of [always] for container sha256:c346e671eac589538f0d90fe3835e994540bf7d2bb7e8d857e616c1f4b0e1620[0;m
2026-08-19T19:55:47.301591Z 01O Running on runner-lylwumykw-project-35-concurrent-0 via 2e3ee0c93397...
2026-08-19T19:55:47.502639Z 00O section_end:1787169347:prepare_script
[0K
2026-08-19T19:55:47.503621Z 00O+section_start:1787169347:get_sources
[0K
2026-08-19T19:55:47.503632Z 00O+[0K[36;1mGetting source from Git repository[0;m[0;m
2026-08-19T19:55:47.845637Z 01O [32;1mGitaly correlation ID: 01M0DSG39YACA7PRMXC45DHY2F[0;m
2026-08-19T19:55:47.854674Z 01O [32;1mFetching changes...[0;m
2026-08-19T19:55:47.861721Z 01O Reinitialized existing Git repository in /builds/lunarwing/lunarwing2/.git/
2026-08-19T19:55:47.863629Z 01O [32;1mCreated fresh repository.[0;m
2026-08-19T19:55:48.270208Z 01O [32;1mChecking out f3becea8 as detached HEAD (ref is master)...[0;m
2026-08-19T19:55:48.405785Z 01O 
2026-08-19T19:55:48.405796Z 01O [32;1mSkipping Git submodules setup[0;m
2026-08-19T19:55:48.618224Z 00O section_end:1787169348:get_sources
[0K
2026-08-19T19:55:48.620687Z 00O+section_start:1787169348:step_script
[0K
2026-08-19T19:55:48.620710Z 00O+[0K[36;1mExecuting "step_script" stage of the job script[0;m[0;m
2026-08-19T19:55:48.620735Z 00O [0KUsing effective pull policy of [always] for container docker.io/library/python:3.12-slim-bookworm@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b[0;m
2026-08-19T19:55:48.622588Z 00O [0KUsing docker image sha256:a1e028236d55ab8a2ec45d7c693137b5adb5d230baecdbae4f9f75cf51b6ce66 for docker.io/library/python:3.12-slim-bookworm@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b with digest python@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b ...[0;m
2026-08-19T19:55:48.984677Z 01O [32;1m$ bash .gitlab/scripts/bootstrap-shell.sh[0;m
2026-08-19T19:55:49.191652Z 01O Get:1 http://deb.debian.org/debian bookworm InRelease [151 kB]
2026-08-19T19:55:49.303436Z 01O Get:2 http://deb.debian.org/debian bookworm-updates InRelease [55.4 kB]
2026-08-19T19:55:49.348836Z 01O Get:3 http://deb.debian.org/debian-security bookworm-security InRelease [34.8 kB]
2026-08-19T19:55:49.486395Z 01O Get:4 http://deb.debian.org/debian bookworm/main amd64 Packages [8790 kB]
2026-08-19T19:55:49.747908Z 01O Get:5 http://deb.debian.org/debian bookworm-updates/main amd64 Packages [6924 B]
2026-08-19T19:55:49.788888Z 01O Get:6 http://deb.debian.org/debian-security bookworm-security/main amd64 Packages [338 kB]
2026-08-19T19:55:50.765553Z 01O Fetched 9376 kB in 2s (5551 kB/s)
2026-08-19T19:55:50.765567Z 01O Reading package lists...
2026-08-19T19:55:51.286797Z 01O Reading package lists...
2026-08-19T19:55:51.825607Z 01O Building dependency tree...
2026-08-19T19:55:51.974873Z 01O Reading state information...
2026-08-19T19:55:52.112165Z 01O bash is already the newest version (5.2.15-2+b13).
2026-08-19T19:55:52.112178Z 01O bash set to manually installed.
2026-08-19T19:55:52.112180Z 01O coreutils is already the newest version (9.1-1).
2026-08-19T19:55:52.112181Z 01O coreutils set to manually installed.
2026-08-19T19:55:52.112183Z 01O gzip is already the newest version (1.12-1).
2026-08-19T19:55:52.112184Z 01O gzip set to manually installed.
2026-08-19T19:55:52.112186Z 01O tar is already the newest version (1.34+dfsg-1.2+deb12u1).
2026-08-19T19:55:52.112187Z 01O tar set to manually installed.
2026-08-19T19:55:52.112189Z 01O util-linux is already the newest version (2.38.1-5+deb12u3).
2026-08-19T19:55:52.112190Z 01O util-linux set to manually installed.
2026-08-19T19:55:52.112192Z 01O The following additional packages will be installed:
2026-08-19T19:55:52.112212Z 01O   git-man libbrotli1 libcurl3-gnutls libcurl4 liberror-perl
2026-08-19T19:55:52.112347Z 01O   libevent-core-2.1-7 libexpat1 libgdbm-compat4 libjq1 libldap-2.5-0
2026-08-19T19:55:52.112714Z 01O   libnghttp2-14 libonig5 libperl5.36 libproc2-0 libpsl5 librtmp1 libsasl2-2
2026-08-19T19:55:52.113214Z 01O   libsasl2-modules-db libssh2-1 libutempter0 p7zip perl perl-modules-5.36
2026-08-19T19:55:52.114314Z 01O Suggested packages:
2026-08-19T19:55:52.114316Z 01O   gettext-base git-daemon-run | git-daemon-sysvinit git-doc git-email git-gui
2026-08-19T19:55:52.114318Z 01O   gitk gitweb git-cvs git-mediawiki git-svn sensible-utils p7zip-rar perl-doc
2026-08-19T19:55:52.114319Z 01O   libterm-readline-gnu-perl | libterm-readline-perl-perl make
2026-08-19T19:55:52.114321Z 01O   libtap-harness-archive-perl
2026-08-19T19:55:52.114322Z 01O Recommended packages:
2026-08-19T19:55:52.114323Z 01O   patch less ssh-client libldap-common publicsuffix libsasl2-modules psmisc
2026-08-19T19:55:52.280912Z 01O The following NEW packages will be installed:
2026-08-19T19:55:52.280932Z 01O   curl git git-man jq libbrotli1 libcurl3-gnutls libcurl4 liberror-perl
2026-08-19T19:55:52.280938Z 01O   libevent-core-2.1-7 libexpat1 libgdbm-compat4 libjq1 libldap-2.5-0
2026-08-19T19:55:52.280944Z 01O   libnghttp2-14 libonig5 libperl5.36 libproc2-0 libpsl5 librtmp1 libsasl2-2
2026-08-19T19:55:52.280949Z 01O   libsasl2-modules-db libssh2-1 libutempter0 p7zip p7zip-full perl
2026-08-19T19:55:52.281027Z 01O   perl-modules-5.36 procps shellcheck tmux
2026-08-19T19:55:52.281034Z 01O The following packages will be upgraded:
2026-08-19T19:55:52.281038Z 01O   ca-certificates
2026-08-19T19:55:52.391415Z 01O 1 upgraded, 30 newly installed, 0 to remove and 0 not upgraded.
2026-08-19T19:55:52.391427Z 01O Need to get 25.2 MB of archives.
2026-08-19T19:55:52.391429Z 01O After this operation, 133 MB of additional disk space will be used.
2026-08-19T19:55:52.391431Z 01O Get:1 http://deb.debian.org/debian bookworm/main amd64 perl-modules-5.36 all 5.36.0-7+deb12u3 [2815 kB]
2026-08-19T19:55:52.632701Z 01O Get:2 http://deb.debian.org/debian bookworm/main amd64 libgdbm-compat4 amd64 1.23-3 [48.2 kB]
2026-08-19T19:55:52.634134Z 01O Get:3 http://deb.debian.org/debian bookworm/main amd64 libperl5.36 amd64 5.36.0-7+deb12u3 [4196 kB]
2026-08-19T19:55:52.719560Z 01O Get:4 http://deb.debian.org/debian bookworm/main amd64 perl amd64 5.36.0-7+deb12u3 [239 kB]
2026-08-19T19:55:52.723366Z 01O Get:5 http://deb.debian.org/debian bookworm/main amd64 libproc2-0 amd64 2:4.0.2-3 [62.8 kB]
2026-08-19T19:55:52.763788Z 01O Get:6 http://deb.debian.org/debian bookworm/main amd64 procps amd64 2:4.0.2-3 [709 kB]
2026-08-19T19:55:52.775318Z 01O Get:7 http://deb.debian.org/debian-security bookworm-security/main amd64 ca-certificates all 20250419~deb12u1 [162 kB]
2026-08-19T19:55:52.777217Z 01O Get:8 http://deb.debian.org/debian bookworm/main amd64 libbrotli1 amd64 1.0.9-2+b6 [275 kB]
2026-08-19T19:55:52.781152Z 01O Get:9 http://deb.debian.org/debian bookworm/main amd64 libsasl2-modules-db amd64 2.1.28+dfsg-10 [20.3 kB]
2026-08-19T19:55:52.781826Z 01O Get:10 http://deb.debian.org/debian bookworm/main amd64 libsasl2-2 amd64 2.1.28+dfsg-10 [59.7 kB]
2026-08-19T19:55:52.783155Z 01O Get:11 http://deb.debian.org/debian bookworm/main amd64 libldap-2.5-0 amd64 2.5.13+dfsg-5 [183 kB]
2026-08-19T19:55:52.785874Z 01O Get:12 http://deb.debian.org/debian bookworm/main amd64 libnghttp2-14 amd64 1.52.0-1+deb12u3 [72.4 kB]
2026-08-19T19:55:52.787227Z 01O Get:13 http://deb.debian.org/debian bookworm/main amd64 libpsl5 amd64 0.21.2-1 [58.7 kB]
2026-08-19T19:55:52.788463Z 01O Get:14 http://deb.debian.org/debian bookworm/main amd64 librtmp1 amd64 2.4+20151223.gitfa8646d.1-2+b2 [60.8 kB]
2026-08-19T19:55:52.789678Z 01O Get:15 http://deb.debian.org/debian bookworm/main amd64 libssh2-1 amd64 1.10.0-3+b1 [179 kB]
2026-08-19T19:55:52.814768Z 01O Get:16 http://deb.debian.org/debian bookworm/main amd64 libcurl4 amd64 7.88.1-10+deb12u15 [392 kB]
2026-08-19T19:55:52.846210Z 01O Get:17 http://deb.debian.org/debian bookworm/main amd64 curl amd64 7.88.1-10+deb12u15 [316 kB]
2026-08-19T19:55:52.852013Z 01O Get:18 http://deb.debian.org/debian bookworm/main amd64 libcurl3-gnutls amd64 7.88.1-10+deb12u15 [386 kB]
2026-08-19T19:55:52.858972Z 01O Get:19 http://deb.debian.org/debian bookworm/main amd64 libexpat1 amd64 2.5.0-1+deb12u2 [99.9 kB]
2026-08-19T19:55:52.860719Z 01O Get:20 http://deb.debian.org/debian bookworm/main amd64 liberror-perl all 0.17029-2 [29.0 kB]
2026-08-19T19:55:52.861570Z 01O Get:21 http://deb.debian.org/debian bookworm/main amd64 git-man all 1:2.39.5-0+deb12u3 [2053 kB]
2026-08-19T19:55:52.892708Z 01O Get:22 http://deb.debian.org/debian bookworm/main amd64 git amd64 1:2.39.5-0+deb12u3 [7264 kB]
2026-08-19T19:55:53.003619Z 01O Get:23 http://deb.debian.org/debian bookworm/main amd64 libonig5 amd64 6.9.8-1 [188 kB]
2026-08-19T19:55:53.006437Z 01O Get:24 http://deb.debian.org/debian bookworm/main amd64 libjq1 amd64 1.6-2.1+deb12u2 [136 kB]
2026-08-19T19:55:53.008391Z 01O Get:25 http://deb.debian.org/debian bookworm/main amd64 jq amd64 1.6-2.1+deb12u2 [64.0 kB]
2026-08-19T19:55:53.039873Z 01O Get:26 http://deb.debian.org/debian bookworm/main amd64 libevent-core-2.1-7 amd64 2.1.12-stable-8 [131 kB]
2026-08-19T19:55:53.042367Z 01O Get:27 http://deb.debian.org/debian bookworm/main amd64 libutempter0 amd64 1.2.1-3 [8960 B]
2026-08-19T19:55:53.042710Z 01O Get:28 http://deb.debian.org/debian-security bookworm-security/main amd64 p7zip amd64 16.02+really26.02+dfsg-0+deb12u1 [453 kB]
2026-08-19T19:55:53.077587Z 01O Get:29 http://deb.debian.org/debian-security bookworm-security/main amd64 p7zip-full amd64 16.02+really26.02+dfsg-0+deb12u1 [1423 kB]
2026-08-19T19:55:53.117017Z 01O Get:30 http://deb.debian.org/debian bookworm/main amd64 shellcheck amd64 0.9.0-1 [2621 kB]
2026-08-19T19:55:53.153738Z 01O Get:31 http://deb.debian.org/debian bookworm/main amd64 tmux amd64 3.3a-3 [455 kB]
2026-08-19T19:55:53.430739Z 01E debconf: delaying package configuration, since apt-utils is not installed
2026-08-19T19:55:53.482413Z 01O Fetched 25.2 MB in 1s (28.8 MB/s)
2026-08-19T19:55:53.517845Z 01O Selecting previously unselected package perl-modules-5.36.
2026-08-19T19:55:53.517856Z 01O (Reading database ... 
2026-08-19T19:55:53.519461Z 01O+(Reading database ... 5%
(Reading database ... 10%
(Reading database ... 15%
(Reading database ... 20%
(Reading database ... 25%
(Reading database ... 30%
(Reading database ... 35%
(Reading database ... 40%
(Reading database ... 45%
(Reading database ... 50%
(Reading database ... 55%
(Reading database ... 60%
(Reading database ... 65%
(Reading database ... 70%
2026-08-19T19:55:53.521998Z 01O+(Reading database ... 75%
2026-08-19T19:55:53.531022Z 01O+(Reading database ... 80%
2026-08-19T19:55:53.537479Z 01O+(Reading database ... 85%
2026-08-19T19:55:53.544473Z 01O+(Reading database ... 90%
2026-08-19T19:55:53.551093Z 01O+(Reading database ... 95%
2026-08-19T19:55:53.559420Z 01O+(Reading database ... 100%
(Reading database ... 6695 files and directories currently installed.)
2026-08-19T19:55:53.559857Z 01O Preparing to unpack .../00-perl-modules-5.36_5.36.0-7+deb12u3_all.deb ...
2026-08-19T19:55:53.562289Z 01O Unpacking perl-modules-5.36 (5.36.0-7+deb12u3) ...
2026-08-19T19:55:53.880528Z 01O Selecting previously unselected package libgdbm-compat4:amd64.
2026-08-19T19:55:53.880540Z 01O Preparing to unpack .../01-libgdbm-compat4_1.23-3_amd64.deb ...
2026-08-19T19:55:53.883070Z 01O Unpacking libgdbm-compat4:amd64 (1.23-3) ...
2026-08-19T19:55:53.915856Z 01O Selecting previously unselected package libperl5.36:amd64.
2026-08-19T19:55:53.916924Z 01O Preparing to unpack .../02-libperl5.36_5.36.0-7+deb12u3_amd64.deb ...
2026-08-19T19:55:53.918088Z 01O Unpacking libperl5.36:amd64 (5.36.0-7+deb12u3) ...
2026-08-19T19:55:54.377816Z 01O Selecting previously unselected package perl.
2026-08-19T19:55:54.378216Z 01O Preparing to unpack .../03-perl_5.36.0-7+deb12u3_amd64.deb ...
2026-08-19T19:55:54.397085Z 01O Unpacking perl (5.36.0-7+deb12u3) ...
2026-08-19T19:55:54.464003Z 01O Selecting previously unselected package libproc2-0:amd64.
2026-08-19T19:55:54.464019Z 01O Preparing to unpack .../04-libproc2-0_2%3a4.0.2-3_amd64.deb ...
2026-08-19T19:55:54.464424Z 01O Unpacking libproc2-0:amd64 (2:4.0.2-3) ...
2026-08-19T19:55:54.489044Z 01O Selecting previously unselected package procps.
2026-08-19T19:55:54.489062Z 01O Preparing to unpack .../05-procps_2%3a4.0.2-3_amd64.deb ...
2026-08-19T19:55:54.501101Z 01O Unpacking procps (2:4.0.2-3) ...
2026-08-19T19:55:54.586302Z 01O Preparing to unpack .../06-ca-certificates_20250419~deb12u1_all.deb ...
2026-08-19T19:55:54.589192Z 01O Unpacking ca-certificates (20250419~deb12u1) over (20230311+deb12u1) ...
2026-08-19T19:55:54.671974Z 01O Selecting previously unselected package libbrotli1:amd64.
2026-08-19T19:55:54.671986Z 01O Preparing to unpack .../07-libbrotli1_1.0.9-2+b6_amd64.deb ...
2026-08-19T19:55:54.672678Z 01O Unpacking libbrotli1:amd64 (1.0.9-2+b6) ...
2026-08-19T19:55:54.709055Z 01O Selecting previously unselected package libsasl2-modules-db:amd64.
2026-08-19T19:55:54.710346Z 01O Preparing to unpack .../08-libsasl2-modules-db_2.1.28+dfsg-10_amd64.deb ...
2026-08-19T19:55:54.710836Z 01O Unpacking libsasl2-modules-db:amd64 (2.1.28+dfsg-10) ...
2026-08-19T19:55:54.726492Z 01O Selecting previously unselected package libsasl2-2:amd64.
2026-08-19T19:55:54.727841Z 01O Preparing to unpack .../09-libsasl2-2_2.1.28+dfsg-10_amd64.deb ...
2026-08-19T19:55:54.728886Z 01O Unpacking libsasl2-2:amd64 (2.1.28+dfsg-10) ...
2026-08-19T19:55:54.750321Z 01O Selecting previously unselected package libldap-2.5-0:amd64.
2026-08-19T19:55:54.752701Z 01O Preparing to unpack .../10-libldap-2.5-0_2.5.13+dfsg-5_amd64.deb ...
2026-08-19T19:55:54.752831Z 01O Unpacking libldap-2.5-0:amd64 (2.5.13+dfsg-5) ...
2026-08-19T19:55:54.786854Z 01O Selecting previously unselected package libnghttp2-14:amd64.
2026-08-19T19:55:54.786863Z 01O Preparing to unpack .../11-libnghttp2-14_1.52.0-1+deb12u3_amd64.deb ...
2026-08-19T19:55:54.788248Z 01O Unpacking libnghttp2-14:amd64 (1.52.0-1+deb12u3) ...
2026-08-19T19:55:54.813076Z 01O Selecting previously unselected package libpsl5:amd64.
2026-08-19T19:55:54.816597Z 01O Preparing to unpack .../12-libpsl5_0.21.2-1_amd64.deb ...
2026-08-19T19:55:54.817355Z 01O Unpacking libpsl5:amd64 (0.21.2-1) ...
2026-08-19T19:55:54.841753Z 01O Selecting previously unselected package librtmp1:amd64.
2026-08-19T19:55:54.843264Z 01O Preparing to unpack .../13-librtmp1_2.4+20151223.gitfa8646d.1-2+b2_amd64.deb ...
2026-08-19T19:55:54.844098Z 01O Unpacking librtmp1:amd64 (2.4+20151223.gitfa8646d.1-2+b2) ...
2026-08-19T19:55:54.883798Z 01O Selecting previously unselected package libssh2-1:amd64.
2026-08-19T19:55:54.883807Z 01O Preparing to unpack .../14-libssh2-1_1.10.0-3+b1_amd64.deb ...
2026-08-19T19:55:54.884524Z 01O Unpacking libssh2-1:amd64 (1.10.0-3+b1) ...
2026-08-19T19:55:54.918771Z 01O Selecting previously unselected package libcurl4:amd64.
2026-08-19T19:55:54.922127Z 01O Preparing to unpack .../15-libcurl4_7.88.1-10+deb12u15_amd64.deb ...
2026-08-19T19:55:54.923688Z 01O Unpacking libcurl4:amd64 (7.88.1-10+deb12u15) ...
2026-08-19T19:55:54.983197Z 01O Selecting previously unselected package curl.
2026-08-19T19:55:54.986946Z 01O Preparing to unpack .../16-curl_7.88.1-10+deb12u15_amd64.deb ...
2026-08-19T19:55:54.987696Z 01O Unpacking curl (7.88.1-10+deb12u15) ...
2026-08-19T19:55:55.039805Z 01O Selecting previously unselected package libcurl3-gnutls:amd64.
2026-08-19T19:55:55.042080Z 01O Preparing to unpack .../17-libcurl3-gnutls_7.88.1-10+deb12u15_amd64.deb ...
2026-08-19T19:55:55.043392Z 01O Unpacking libcurl3-gnutls:amd64 (7.88.1-10+deb12u15) ...
2026-08-19T19:55:55.086049Z 01O Selecting previously unselected package libexpat1:amd64.
2026-08-19T19:55:55.088297Z 01O Preparing to unpack .../18-libexpat1_2.5.0-1+deb12u2_amd64.deb ...
2026-08-19T19:55:55.089557Z 01O Unpacking libexpat1:amd64 (2.5.0-1+deb12u2) ...
2026-08-19T19:55:55.137406Z 01O Selecting previously unselected package liberror-perl.
2026-08-19T19:55:55.139640Z 01O Preparing to unpack .../19-liberror-perl_0.17029-2_all.deb ...
2026-08-19T19:55:55.140756Z 01O Unpacking liberror-perl (0.17029-2) ...
2026-08-19T19:55:55.171170Z 01O Selecting previously unselected package git-man.
2026-08-19T19:55:55.173441Z 01O Preparing to unpack .../20-git-man_1%3a2.39.5-0+deb12u3_all.deb ...
2026-08-19T19:55:55.174661Z 01O Unpacking git-man (1:2.39.5-0+deb12u3) ...
2026-08-19T19:55:55.326823Z 01O Selecting previously unselected package git.
2026-08-19T19:55:55.328388Z 01O Preparing to unpack .../21-git_1%3a2.39.5-0+deb12u3_amd64.deb ...
2026-08-19T19:55:55.342725Z 01O Unpacking git (1:2.39.5-0+deb12u3) ...
2026-08-19T19:55:55.724635Z 01O Selecting previously unselected package libonig5:amd64.
2026-08-19T19:55:55.726338Z 01O Preparing to unpack .../22-libonig5_6.9.8-1_amd64.deb ...
2026-08-19T19:55:55.727807Z 01O Unpacking libonig5:amd64 (6.9.8-1) ...
2026-08-19T19:55:55.794566Z 01O Selecting previously unselected package libjq1:amd64.
2026-08-19T19:55:55.796382Z 01O Preparing to unpack .../23-libjq1_1.6-2.1+deb12u2_amd64.deb ...
2026-08-19T19:55:55.797732Z 01O Unpacking libjq1:amd64 (1.6-2.1+deb12u2) ...
2026-08-19T19:55:55.845054Z 01O Selecting previously unselected package jq.
2026-08-19T19:55:55.847750Z 01O Preparing to unpack .../24-jq_1.6-2.1+deb12u2_amd64.deb ...
2026-08-19T19:55:55.849814Z 01O Unpacking jq (1.6-2.1+deb12u2) ...
2026-08-19T19:55:55.898663Z 01O Selecting previously unselected package libevent-core-2.1-7:amd64.
2026-08-19T19:55:55.902045Z 01O Preparing to unpack .../25-libevent-core-2.1-7_2.1.12-stable-8_amd64.deb ...
2026-08-19T19:55:55.907540Z 01O Unpacking libevent-core-2.1-7:amd64 (2.1.12-stable-8) ...
2026-08-19T19:55:55.969035Z 01O Selecting previously unselected package libutempter0:amd64.
2026-08-19T19:55:55.970386Z 01O Preparing to unpack .../26-libutempter0_1.2.1-3_amd64.deb ...
2026-08-19T19:55:55.972175Z 01O Unpacking libutempter0:amd64 (1.2.1-3) ...
2026-08-19T19:55:56.005106Z 01O Selecting previously unselected package p7zip.
2026-08-19T19:55:56.007497Z 01O Preparing to unpack .../27-p7zip_16.02+really26.02+dfsg-0+deb12u1_amd64.deb ...
2026-08-19T19:55:56.009416Z 01O Unpacking p7zip (16.02+really26.02+dfsg-0+deb12u1) ...
2026-08-19T19:55:56.092714Z 01O Selecting previously unselected package p7zip-full.
2026-08-19T19:55:56.095279Z 01O Preparing to unpack .../28-p7zip-full_16.02+really26.02+dfsg-0+deb12u1_amd64.deb ...
2026-08-19T19:55:56.107142Z 01O Unpacking p7zip-full (16.02+really26.02+dfsg-0+deb12u1) ...
2026-08-19T19:55:56.282995Z 01O Selecting previously unselected package shellcheck.
2026-08-19T19:55:56.285325Z 01O Preparing to unpack .../29-shellcheck_0.9.0-1_amd64.deb ...
2026-08-19T19:55:56.290267Z 01O Unpacking shellcheck (0.9.0-1) ...
2026-08-19T19:55:56.562312Z 01O Selecting previously unselected package tmux.
2026-08-19T19:55:56.562629Z 01O Preparing to unpack .../30-tmux_3.3a-3_amd64.deb ...
2026-08-19T19:55:56.565153Z 01O Unpacking tmux (3.3a-3) ...
2026-08-19T19:55:56.661544Z 01O Setting up libexpat1:amd64 (2.5.0-1+deb12u2) ...
2026-08-19T19:55:56.664808Z 01O Setting up libpsl5:amd64 (0.21.2-1) ...
2026-08-19T19:55:56.667445Z 01O Setting up libbrotli1:amd64 (1.0.9-2+b6) ...
2026-08-19T19:55:56.670305Z 01O Setting up libnghttp2-14:amd64 (1.52.0-1+deb12u3) ...
2026-08-19T19:55:56.673191Z 01O Setting up shellcheck (0.9.0-1) ...
2026-08-19T19:55:56.676063Z 01O Setting up libsasl2-modules-db:amd64 (2.1.28+dfsg-10) ...
2026-08-19T19:55:56.678918Z 01O Setting up ca-certificates (20250419~deb12u1) ...
2026-08-19T19:55:59.647501Z 01O Updating certificates in /etc/ssl/certs...
2026-08-19T19:56:01.337059Z 01O rehash: warning: skipping ca-certificates.crt,it does not contain exactly one certificate or CRL
2026-08-19T19:56:01.342688Z 01O 21 added, 13 removed; done.
2026-08-19T19:56:01.360179Z 01O Setting up perl-modules-5.36 (5.36.0-7+deb12u3) ...
2026-08-19T19:56:01.362315Z 01O Setting up librtmp1:amd64 (2.4+20151223.gitfa8646d.1-2+b2) ...
2026-08-19T19:56:01.364752Z 01O Setting up libproc2-0:amd64 (2:4.0.2-3) ...
2026-08-19T19:56:01.376347Z 01O Setting up libevent-core-2.1-7:amd64 (2.1.12-stable-8) ...
2026-08-19T19:56:01.386574Z 01O Setting up libgdbm-compat4:amd64 (1.23-3) ...
2026-08-19T19:56:01.388674Z 01O Setting up libsasl2-2:amd64 (2.1.28+dfsg-10) ...
2026-08-19T19:56:01.390723Z 01O Setting up libutempter0:amd64 (1.2.1-3) ...
2026-08-19T19:56:01.392944Z 01O Setting up procps (2:4.0.2-3) ...
2026-08-19T19:56:01.419386Z 01O Setting up git-man (1:2.39.5-0+deb12u3) ...
2026-08-19T19:56:01.421411Z 01O Setting up libssh2-1:amd64 (1.10.0-3+b1) ...
2026-08-19T19:56:01.423630Z 01O Setting up tmux (3.3a-3) ...
2026-08-19T19:56:01.438695Z 01O Setting up libperl5.36:amd64 (5.36.0-7+deb12u3) ...
2026-08-19T19:56:01.440691Z 01O Setting up libonig5:amd64 (6.9.8-1) ...
2026-08-19T19:56:01.442838Z 01O Setting up p7zip (16.02+really26.02+dfsg-0+deb12u1) ...
2026-08-19T19:56:01.444859Z 01O Setting up libjq1:amd64 (1.6-2.1+deb12u2) ...
2026-08-19T19:56:01.447018Z 01O Setting up p7zip-full (16.02+really26.02+dfsg-0+deb12u1) ...
2026-08-19T19:56:01.454582Z 01O Setting up libldap-2.5-0:amd64 (2.5.13+dfsg-5) ...
2026-08-19T19:56:01.456978Z 01O Setting up perl (5.36.0-7+deb12u3) ...
2026-08-19T19:56:01.467381Z 01O Setting up jq (1.6-2.1+deb12u2) ...
2026-08-19T19:56:01.470211Z 01O Setting up libcurl4:amd64 (7.88.1-10+deb12u15) ...
2026-08-19T19:56:01.472972Z 01O Setting up curl (7.88.1-10+deb12u15) ...
2026-08-19T19:56:01.475931Z 01O Setting up libcurl3-gnutls:amd64 (7.88.1-10+deb12u15) ...
2026-08-19T19:56:01.478685Z 01O Setting up liberror-perl (0.17029-2) ...
2026-08-19T19:56:01.481614Z 01O Setting up git (1:2.39.5-0+deb12u3) ...
2026-08-19T19:56:01.490110Z 01O Processing triggers for libc-bin (2.36-9+deb12u14) ...
2026-08-19T19:56:01.520571Z 01O Processing triggers for ca-certificates (20250419~deb12u1) ...
2026-08-19T19:56:01.526031Z 01O Updating certificates in /etc/ssl/certs...
2026-08-19T19:56:02.133701Z 01O 0 added, 0 removed; done.
2026-08-19T19:56:02.134031Z 01O Running hooks in /etc/ca-certificates/update.d...
2026-08-19T19:56:02.135817Z 01O done.
2026-08-19T19:56:02.156996Z 01O [32;1m$ bash ic/scripts/check-boundaries.sh[0;m
2026-08-19T19:56:02.164574Z 01O === Architecture Boundary Checks ===
2026-08-19T19:56:02.164580Z 01O 
2026-08-19T19:56:02.164581Z 01O --- Check 1: Direct database driver usage outside db layer ---
2026-08-19T19:56:02.305678Z 01O WARNING: Direct database driver usage found outside db layer:
2026-08-19T19:56:02.306304Z 01O src/agent/compaction.rs:365:        use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306308Z 01O src/agent/reflex.rs:1286:        use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306310Z 01O src/agent/reflex.rs:1800:        use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306311Z 01O src/agent/reflex.rs:1836:        use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306312Z 01O src/agent/reflex.rs:1898:        use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306313Z 01O src/agent/reflex.rs:1957:        use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306314Z 01O src/agent/reflex.rs:1999:        use crate::db::libsql::{LibSqlBackend, fmt_ts};
2026-08-19T19:56:02.306315Z 01O src/agent/reflex.rs:2001:        use libsql::params;
2026-08-19T19:56:02.306316Z 01O src/agent/reflex.rs:2038:        use crate::db::libsql::{LibSqlBackend, fmt_ts};
2026-08-19T19:56:02.306317Z 01O src/agent/reflex.rs:2040:        use libsql::params;
2026-08-19T19:56:02.306318Z 01O src/agent/reflex.rs:2106:        use crate::db::libsql::{LibSqlBackend, fmt_ts};
2026-08-19T19:56:02.306319Z 01O src/agent/reflex.rs:2108:        use libsql::params;
2026-08-19T19:56:02.306320Z 01O src/agent/reflex.rs:2143:        use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306321Z 01O src/bridge/store_adapter.rs:1986:        use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306322Z 01O src/bridge/store_adapter.rs:2054:        use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306323Z 01O src/bridge/store_adapter.rs:2175:        use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306324Z 01O src/bridge/store_adapter.rs:2264:        use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306325Z 01O src/bridge/store_adapter.rs:2354:        use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306326Z 01O src/bridge/store_adapter.rs:2459:        use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306327Z 01O src/bridge/store_adapter.rs:2534:        use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306328Z 01O src/bridge/store_adapter.rs:2641:        use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306329Z 01O src/channels/wasm/storage.rs:322:    row: &tokio_postgres::Row,
2026-08-19T19:56:02.306330Z 01O src/channels/wasm/storage.rs:346:    db: std::sync::Arc<libsql::Database>,
2026-08-19T19:56:02.306330Z 01O src/channels/wasm/storage.rs:351:    pub fn new(db: std::sync::Arc<libsql::Database>) -> Self {
2026-08-19T19:56:02.306332Z 01O src/channels/wasm/storage.rs:355:    async fn connect(&self) -> Result<libsql::Connection, WasmChannelStoreError> {
2026-08-19T19:56:02.306333Z 01O src/channels/wasm/storage.rs:389:            libsql::params![params.user_id.as_str(), params.name.as_str()],
2026-08-19T19:56:02.306334Z 01O src/channels/wasm/storage.rs:402:            libsql::params![
2026-08-19T19:56:02.306335Z 01O src/channels/wasm/storage.rs:409:                libsql::Value::Blob(params.wasm_binary),
2026-08-19T19:56:02.306336Z 01O src/channels/wasm/storage.rs:410:                libsql::Value::Blob(binary_hash),
2026-08-19T19:56:02.306364Z 01O src/channels/wasm/storage.rs:427:                libsql::params![params.user_id.as_str(), params.name.as_str()],
2026-08-19T19:56:02.306365Z 01O src/channels/wasm/storage.rs:463:                libsql::params![user_id, name],
2026-08-19T19:56:02.306366Z 01O src/channels/wasm/storage.rs:493:                libsql::params![user_id, name],
2026-08-19T19:56:02.306367Z 01O src/channels/wasm/storage.rs:543:                libsql::params![user_id],
2026-08-19T19:56:02.306368Z 01O src/channels/wasm/storage.rs:564:                libsql::params![user_id, name],
2026-08-19T19:56:02.306369Z 01O src/channels/wasm/storage.rs:575:fn libsql_channel_opt_text(s: Option<&str>) -> libsql::Value {
2026-08-19T19:56:02.306370Z 01O src/channels/wasm/storage.rs:577:        Some(s) => libsql::Value::Text(s.to_string()),
2026-08-19T19:56:02.306371Z 01O src/channels/wasm/storage.rs:578:        None => libsql::Value::Null,
2026-08-19T19:56:02.306372Z 01O src/channels/wasm/storage.rs:603:fn libsql_row_to_channel(row: &libsql::Row) -> Result<StoredWasmChannel, WasmChannelStoreError> {
2026-08-19T19:56:02.306374Z 01O src/channels/wasm/storage.rs:650:    row: &libsql::Row,
2026-08-19T19:56:02.306375Z 01O src/channels/wasm/wrapper.rs:5620:            crate::db::libsql::LibSqlBackend::new_local(&dir.path().join("owner-route.db"))
2026-08-19T19:56:02.306376Z 01O src/channels/web/tests/multi_tenant.rs:141:    let backend = crate::db::libsql::LibSqlBackend::new_local(&path)
2026-08-19T19:56:02.306377Z 01O src/extensions/manager.rs:9341:            crate::db::libsql::LibSqlBackend::new_local(&db_path)
2026-08-19T19:56:02.306378Z 01O src/extensions/manager.rs:9433:            crate::db::libsql::LibSqlBackend::new_local(&dir.path().join("owner-actor.db"))
2026-08-19T19:56:02.306380Z 01O src/history/store.rs:1522:fn row_to_routine(row: &tokio_postgres::Row) -> Result<Routine, DatabaseError> {
2026-08-19T19:56:02.306381Z 01O src/history/store.rs:1573:fn row_to_routine_run(row: &tokio_postgres::Row) -> Result<RoutineRun, DatabaseError> {
2026-08-19T19:56:02.306382Z 01O src/history/store.rs:2899:fn row_to_user(row: &tokio_postgres::Row) -> UserRecord {
2026-08-19T19:56:02.306383Z 01O src/history/store.rs:2915:fn row_to_api_token(row: &tokio_postgres::Row) -> ApiTokenRecord {
2026-08-19T19:56:02.306384Z 01O src/import/openclaw/reader.rs:85:async fn open_sqlite(db_path: &Path) -> Result<libsql::Connection, ImportError> {
2026-08-19T19:56:02.306385Z 01O src/import/openclaw/reader.rs:86:    let db = libsql::Builder::new_local(db_path)
2026-08-19T19:56:02.306386Z 01O src/import/openclaw/reader.rs:325:                    libsql::params![id.as_str()],
2026-08-19T19:56:02.306387Z 01O src/secrets/store.rs:296:fn row_to_secret(row: &tokio_postgres::Row) -> Secret {
2026-08-19T19:56:02.306388Z 01O src/secrets/store.rs:320:    db: Arc<libsql::Database>,
2026-08-19T19:56:02.306389Z 01O src/secrets/store.rs:327:    pub fn new(db: Arc<libsql::Database>, crypto: Arc<SecretsCrypto>) -> Self {
2026-08-19T19:56:02.306390Z 01O src/secrets/store.rs:331:    async fn connect(&self) -> Result<libsql::Connection, SecretError> {
2026-08-19T19:56:02.306391Z 01O src/secrets/store.rs:379:                libsql::params![
2026-08-19T19:56:02.306392Z 01O src/secrets/store.rs:383:                    libsql::Value::Blob(encrypted_value.clone()),
2026-08-19T19:56:02.306393Z 01O src/secrets/store.rs:384:                    libsql::Value::Blob(key_salt.clone()),
2026-08-19T19:56:02.306394Z 01O src/secrets/store.rs:402:                libsql::params![user_id, params.name.as_str()],
2026-08-19T19:56:02.306395Z 01O src/secrets/store.rs:433:                libsql::params![user_id, name.as_str()],
2026-08-19T19:56:02.306396Z 01O src/secrets/store.rs:474:                libsql::params![user_id, name.as_str()],
2026-08-19T19:56:02.306397Z 01O src/secrets/store.rs:491:                libsql::params![user_id],
2026-08-19T19:56:02.306398Z 01O src/secrets/store.rs:516:                libsql::params![user_id, name.as_str()],
2026-08-19T19:56:02.306406Z 01O src/secrets/store.rs:534:            libsql::params![now.as_str(), secret_id.to_string()],
2026-08-19T19:56:02.306407Z 01O src/secrets/store.rs:571:fn libsql_opt_text(s: Option<&str>) -> libsql::Value {
2026-08-19T19:56:02.306408Z 01O src/secrets/store.rs:573:        Some(s) => libsql::Value::Text(s.to_string()),
2026-08-19T19:56:02.306409Z 01O src/secrets/store.rs:574:        None => libsql::Value::Null,
2026-08-19T19:56:02.306410Z 01O src/secrets/store.rs:596:fn libsql_row_to_secret(row: &libsql::Row) -> Result<Secret, SecretError> {
2026-08-19T19:56:02.306411Z 01O src/testing/mod.rs:51:    use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306412Z 01O src/tools/builtin/memory.rs:1252:                    tokio_postgres::Config::new(),
2026-08-19T19:56:02.306413Z 01O src/tools/builtin/memory.rs:1253:                    tokio_postgres::NoTls,
2026-08-19T19:56:02.306414Z 01O src/tools/builtin/memory.rs:1363:                    tokio_postgres::Config::new(),
2026-08-19T19:56:02.306415Z 01O src/tools/builtin/memory.rs:1364:                    tokio_postgres::NoTls,
2026-08-19T19:56:02.306416Z 01O src/tools/builtin/memory.rs:1466:        use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306417Z 01O src/tools/builtin/memory.rs:1823:            use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306418Z 01O src/tools/mcp/config.rs:3298:        let backend = crate::db::libsql::LibSqlBackend::new_local(&dir.path().join("t.db"))
2026-08-19T19:56:02.306419Z 01O src/tools/wasm/storage.rs:566:fn row_to_tool(row: &tokio_postgres::Row) -> Result<StoredWasmTool, WasmStorageError> {
2026-08-19T19:56:02.306420Z 01O src/tools/wasm/storage.rs:596:    db: std::sync::Arc<libsql::Database>,
2026-08-19T19:56:02.306421Z 01O src/tools/wasm/storage.rs:601:    pub fn new(db: std::sync::Arc<libsql::Database>) -> Self {
2026-08-19T19:56:02.306422Z 01O src/tools/wasm/storage.rs:605:    async fn connect(&self) -> Result<libsql::Connection, WasmStorageError> {
2026-08-19T19:56:02.306423Z 01O src/tools/wasm/storage.rs:639:            libsql::params![params.user_id.as_str(), params.name.as_str()],
2026-08-19T19:56:02.306424Z 01O src/tools/wasm/storage.rs:652:            libsql::params![
2026-08-19T19:56:02.306425Z 01O src/tools/wasm/storage.rs:659:                libsql::Value::Blob(params.wasm_binary),
2026-08-19T19:56:02.306426Z 01O src/tools/wasm/storage.rs:660:                libsql::Value::Blob(binary_hash),
2026-08-19T19:56:02.306427Z 01O src/tools/wasm/storage.rs:679:                libsql::params![params.user_id.as_str(), params.name.as_str()],
2026-08-19T19:56:02.306428Z 01O src/tools/wasm/storage.rs:711:                libsql::params![user_id, name],
2026-08-19T19:56:02.306429Z 01O src/tools/wasm/storage.rs:747:                libsql::params![user_id, name],
2026-08-19T19:56:02.306430Z 01O src/tools/wasm/storage.rs:805:                libsql::params![tool_id.to_string()],
2026-08-19T19:56:02.306431Z 01O src/tools/wasm/storage.rs:880:                libsql::params![user_id],
2026-08-19T19:56:02.306432Z 01O src/tools/wasm/storage.rs:908:                libsql::params![status.to_string(), now.as_str(), user_id, name],
2026-08-19T19:56:02.306433Z 01O src/tools/wasm/storage.rs:925:                libsql::params![user_id, name],
2026-08-19T19:56:02.306434Z 01O src/tools/wasm/storage.rs:935:fn libsql_wasm_opt_text(s: Option<&str>) -> libsql::Value {
2026-08-19T19:56:02.306435Z 01O src/tools/wasm/storage.rs:937:        Some(s) => libsql::Value::Text(s.to_string()),
2026-08-19T19:56:02.306436Z 01O src/tools/wasm/storage.rs:938:        None => libsql::Value::Null,
2026-08-19T19:56:02.306437Z 01O src/tools/wasm/storage.rs:964:fn libsql_row_to_tool(row: &libsql::Row) -> Result<StoredWasmTool, WasmStorageError> {
2026-08-19T19:56:02.306438Z 01O src/tools/wasm/storage.rs:974:fn libsql_row_to_tool_with_offset(row: &libsql::Row) -> Result<StoredWasmTool, WasmStorageError> {
2026-08-19T19:56:02.306440Z 01O src/tools/wasm/storage.rs:981:    row: &libsql::Row,
2026-08-19T19:56:02.306446Z 01O src/workspace/hygiene.rs:538:            use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306447Z 01O src/workspace/mod.rs:2466:    use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306448Z 01O src/workspace/mod.rs:2881:        use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306449Z 01O src/workspace/mod.rs:3036:        use crate::db::libsql::LibSqlBackend;
2026-08-19T19:56:02.306460Z 01O 
2026-08-19T19:56:02.309857Z 01O (100 occurrence(s) -- these modules should use the Database trait)
2026-08-19T19:56:02.323263Z 01O 
2026-08-19T19:56:02.323270Z 01O HARD VIOLATION: New direct database driver usage outside db layer:
2026-08-19T19:56:02.323273Z 01O src/tools/mcp/config.rs:3298:        let backend = crate::db::libsql::LibSqlBackend::new_local(&dir.path().join("t.db"))
2026-08-19T19:56:02.323274Z 01O 
2026-08-19T19:56:02.323288Z 01O --- Check 2: .unwrap() / .expect() / assert!() in production code ---
2026-08-19T19:56:02.569778Z 01O WARNING: ~17058 .unwrap()/.expect()/assert!() calls found in src/ (excluding main/testing/setup).
2026-08-19T19:56:02.569791Z 01O Many are in test modules; a per-file breakdown helps triage:
2026-08-19T19:56:02.569794Z 01O 
2026-08-19T19:56:02.625231Z 01O     701 src/darkirc_key_manager.rs
2026-08-19T19:56:02.625242Z 01O     469 src/extensions/manager.rs
2026-08-19T19:56:02.625244Z 01O     307 src/tools/mcp/config.rs
2026-08-19T19:56:02.625246Z 01O     305 src/bridge/router.rs
2026-08-19T19:56:02.625247Z 01O     278 src/channels/xmpp/mod.rs
2026-08-19T19:56:02.625249Z 01O     274 src/llm/reasoning.rs
2026-08-19T19:56:02.625250Z 01O     269 src/settings.rs
2026-08-19T19:56:02.625251Z 01O     255 src/agent/session.rs
2026-08-19T19:56:02.625252Z 01O     245 src/agent/reflex.rs
2026-08-19T19:56:02.625253Z 01O     232 src/testing/mod.rs
2026-08-19T19:56:02.625255Z 01O     228 src/channels/wasm/wrapper.rs
2026-08-19T19:56:02.625256Z 01O     209 src/llm/lunarwing_cloud_chat.rs
2026-08-19T19:56:02.625257Z 01O     207 src/tools/mcp/gateway_control.rs
2026-08-19T19:56:02.625259Z 01O     207 src/channels/signal.rs
2026-08-19T19:56:02.625260Z 01O     190 src/context/manager.rs
2026-08-19T19:56:02.628422Z 01O     ... and 304 more files
2026-08-19T19:56:02.628428Z 01O 
2026-08-19T19:56:02.628429Z 01O (This is a warning for gradual cleanup, not a blocking violation.)
2026-08-19T19:56:02.628432Z 01O (Many of these are inside #[cfg(test)] modules which is acceptable.)
2026-08-19T19:56:02.628433Z 01O 
2026-08-19T19:56:02.628434Z 01O --- Check 3: Direct env var reads outside config layer ---
2026-08-19T19:56:02.659868Z 01O WARNING: Direct env var reads found outside config layer (107 occurrences):
2026-08-19T19:56:02.660672Z 01O src/app.rs:635:                        if let Some(home) = std::env::var_os(STUDY_NOTES_HOME_ENV) {
2026-08-19T19:56:02.660682Z 01O src/app.rs:636:                            let state_home = std::env::var_os(STUDY_NOTES_STATE_HOME_ENV);
2026-08-19T19:56:02.660685Z 01O src/app.rs:638:                                std::env::var_os(STUDY_NOTES_ALLOW_WRITE_ENV).as_deref(),
2026-08-19T19:56:02.660689Z 01O src/app.rs:1135:            let onboard_env = std::env::var("ONBOARD_COMPLETED")
2026-08-19T19:56:02.660692Z 01O src/app.rs:1161:            let import_dir = std::env::var("WORKSPACE_IMPORT_DIR")
2026-08-19T19:56:02.660695Z 01O src/bridge/auth_manager.rs:572:        let original = std::env::var(key).ok();
2026-08-19T19:56:02.660698Z 01O src/bridge/router.rs:34:    std::env::var("ENGINE_V2")
2026-08-19T19:56:02.660701Z 01O src/bridge/router.rs:80:    let configured = std::env::var("ENGINE_V2_CHANNELS").ok();
2026-08-19T19:56:02.660705Z 01O src/bridge/router.rs:2971:        if std::env::var("AGENT_SUPERVISED_MODE")
2026-08-19T19:56:02.660708Z 01O src/bridge/router.rs:4911:    let token = std::env::var("CLAWHUB_TOKEN").map_err(|_| {
2026-08-19T19:56:02.660711Z 01O src/bridge/store_adapter.rs:1569:            let allow = std::env::var("ORCHESTRATOR_SELF_MODIFY")
2026-08-19T19:56:02.660715Z 01O src/channels/coalesce.rs:107:        if let Ok(v) = std::env::var("COALESCE_ENABLED") {
2026-08-19T19:56:02.660764Z 01O src/channels/coalesce.rs:110:        if let Some(window) = std::env::var("COALESCE_WINDOW_SEC")
2026-08-19T19:56:02.660768Z 01O src/channels/coalesce.rs:117:        if let Some(n) = std::env::var("COALESCE_MAX_MERGES")
2026-08-19T19:56:02.660772Z 01O src/channels/coalesce.rs:124:        if let Ok(v) = std::env::var("COORD_CHANNELS") {
2026-08-19T19:56:02.660775Z 01O src/channels/manager.rs:34:            coordination_channels: std::env::var(COORD_CHANNELS_ENV)
2026-08-19T19:56:02.660778Z 01O src/channels/manager.rs:81:    std::env::var(name)
2026-08-19T19:56:02.660847Z 01O src/channels/wasm/bundled.rs:39:        std::env::var("LUNARWING_CHANNELS_SRC").or_else(|_| std::env::var("IRONCLAW_CHANNELS_SRC"))
2026-08-19T19:56:02.660852Z 01O src/channels/wasm/setup.rs:372:            if let Ok(env_value) = std::env::var(&placeholder)
2026-08-19T19:56:02.660855Z 01O src/channels/wasm/setup.rs:443:            && let Ok(value) = std::env::var(env_var)
2026-08-19T19:56:02.660858Z 01O src/channels/wasm/setup.rs:521:        if let Ok(val) = std::env::var(env_var)
2026-08-19T19:56:02.660861Z 01O src/channels/wasm/wrapper.rs:3669:                if std::env::var("CI").is_ok() {
2026-08-19T19:56:02.660864Z 01O src/channels/wasm/wrapper.rs:3685:                if std::env::var("CI").is_ok() {
2026-08-19T19:56:02.660867Z 01O src/channels/wasm/wrapper.rs:3712:        if let Some(home) = std::env::var_os("HOME") {
2026-08-19T19:56:02.660871Z 01O src/channels/web/handlers/coord.rs:224:    std::env::var("COORD_ADMIN_PORT")
2026-08-19T19:56:02.660874Z 01O src/channels/web/handlers/coord.rs:235:    std::env::var(COORD_ADMIN_TOKEN_ENV)
2026-08-19T19:56:02.660877Z 01O src/channels/web/log_layer.rs:179:        std::env::var("RUST_LOG").unwrap_or_else(|_| "lunarwing=info,tower_http=warn".to_string());
2026-08-19T19:56:02.660881Z 01O src/channels/xmpp/mod.rs:3168:    std::env::var("ALLOW_PRIVATE_IPS").ok().as_deref() == Some("1")
2026-08-19T19:56:02.660884Z 01O src/context/state.rs:28:    std::env::var("VISION_SERVICE_URL")
2026-08-19T19:56:02.660887Z 01O src/db/libsql/workspace.rs:34:    let enabled = std::env::var("EMBEDDING_ENABLED")
2026-08-19T19:56:02.660890Z 01O src/db/libsql/workspace.rs:43:    if let Ok(dim_str) = std::env::var("EMBEDDING_DIMENSION")
2026-08-19T19:56:02.660894Z 01O src/db/libsql/workspace.rs:51:        std::env::var("EMBEDDING_MODEL").unwrap_or_else(|_| "text-embedding-3-small".to_string());
2026-08-19T19:56:02.660897Z 01O src/extensions/manager.rs:423:            gateway_token: std::env::var("GATEWAY_AUTH_TOKEN").ok(),
2026-08-19T19:56:02.660901Z 01O src/extensions/manager.rs:991:                    .is_some_and(|v| std::env::var(v).is_ok());
2026-08-19T19:56:02.660904Z 01O src/extensions/manager.rs:3742:            && let Ok(value) = std::env::var(env_var)
2026-08-19T19:56:02.660908Z 01O src/extensions/manager.rs:4057:            && let Ok(val) = std::env::var(env)
2026-08-19T19:56:02.660911Z 01O src/extensions/manager.rs:4359:                    .is_some_and(|e| std::env::var(e).is_ok())
2026-08-19T19:56:02.660914Z 01O src/extensions/manager.rs:4366:                    .is_some_and(|e| std::env::var(e).is_ok())
2026-08-19T19:56:02.660917Z 01O src/extensions/manager.rs:4429:                    .is_some_and(|v| std::env::var(v).is_ok());
2026-08-19T19:56:02.660921Z 01O src/extensions/manager.rs:6342:        if let Ok(value) = std::env::var(placeholder)
2026-08-19T19:56:02.660924Z 01O src/extensions/manager.rs:9893:        let original = std::env::var("IRONCLAW_OAUTH_CALLBACK_URL").ok();
2026-08-19T19:56:02.660927Z 01O src/extensions/manager.rs:9915:        let original = std::env::var("IRONCLAW_OAUTH_CALLBACK_URL").ok();
2026-08-19T19:56:02.660930Z 01O src/extensions/manager.rs:9936:        let original = std::env::var("IRONCLAW_OAUTH_CALLBACK_URL").ok();
2026-08-19T19:56:02.660934Z 01O src/extensions/manager.rs:9962:        let original = std::env::var("IRONCLAW_OAUTH_CALLBACK_URL").ok();
2026-08-19T19:56:02.660937Z 01O src/extensions/manager.rs:9990:            let original = std::env::var("IRONCLAW_OAUTH_CALLBACK_URL").ok();
2026-08-19T19:56:02.660962Z 01O src/extensions/manager.rs:10048:        let original = std::env::var("IRONCLAW_OAUTH_CALLBACK_URL").ok();
2026-08-19T19:56:02.660966Z 01O src/extensions/manager.rs:10074:        let original = std::env::var("IRONCLAW_OAUTH_CALLBACK_URL").ok();
2026-08-19T19:56:02.660969Z 01O src/gate/coordination.rs:42:        let permissive = std::env::var("COORD_PERMISSIVE")
2026-08-19T19:56:02.660972Z 01O src/gate/violation_store.rs:122:    std::env::var(name).ok()?.trim().parse().ok()
2026-08-19T19:56:02.660975Z 01O src/hooks/coordination.rs:259:        if let Ok(v) = std::env::var("COORD_AGENT_ID") {
2026-08-19T19:56:02.660978Z 01O src/hooks/coordination.rs:381:    std::env::var(name)
2026-08-19T19:56:02.660981Z 01O src/hooks/coordination.rs:387:    std::env::var(name)
2026-08-19T19:56:02.660983Z 01O src/hooks/coordination.rs:398:    std::env::var(name).ok()?.trim().parse().ok()
2026-08-19T19:56:02.660986Z 01O src/hooks/coordination_knowledge.rs:113:        if let Ok(raw) = std::env::var("COORD_KNOWLEDGE_PUBLISHERS") {
2026-08-19T19:56:02.660990Z 01O src/hooks/coordination_knowledge.rs:152:    let Ok(raw) = std::env::var(name) else {
2026-08-19T19:56:02.660993Z 01O src/import/openclaw/mod.rs:45:        if let Ok(home) = std::env::var("HOME") {
2026-08-19T19:56:02.660996Z 01O src/llm/lunarwing_cloud_chat.rs:205:        if let Ok(key) = std::env::var("LUNARWING_CLOUD_API_KEY")
2026-08-19T19:56:02.661000Z 01O src/llm/oauth_helpers.rs:395:        let original = std::env::var("OAUTH_CALLBACK_HOST").ok();
2026-08-19T19:56:02.661003Z 01O src/llm/oauth_helpers.rs:419:        let original = std::env::var("OAUTH_CALLBACK_HOST").ok();
2026-08-19T19:56:02.661006Z 01O src/llm/recording.rs:574:        let enabled = std::env::var("LUNARWING_RECORD_TRACE")
2026-08-19T19:56:02.661010Z 01O src/llm/recording.rs:575:            .or_else(|_| std::env::var("IRONCLAW_RECORD_TRACE"))
2026-08-19T19:56:02.661013Z 01O src/llm/recording.rs:580:        let output_path = std::env::var("LUNARWING_TRACE_OUTPUT")
2026-08-19T19:56:02.661016Z 01O src/llm/recording.rs:581:            .or_else(|_| std::env::var("IRONCLAW_TRACE_OUTPUT"))
2026-08-19T19:56:02.661020Z 01O src/llm/recording.rs:590:        let model_name = std::env::var("LUNARWING_TRACE_MODEL_NAME")
2026-08-19T19:56:02.661023Z 01O src/llm/recording.rs:591:            .or_else(|_| std::env::var("IRONCLAW_TRACE_MODEL_NAME"))
2026-08-19T19:56:02.661026Z 01O src/llm/session.rs:619:    if let Ok(token) = std::env::var("LUNARWING_CLOUD_SESSION_TOKEN")
2026-08-19T19:56:02.661030Z 01O src/orchestrator/mod.rs:61:    std::env::var("ORCHESTRATOR_PORT")
2026-08-19T19:56:02.661033Z 01O src/orchestrator/reaper.rs:676:            std::env::var("LUNARWING_E2E_DOCKER_TESTS").is_ok()
2026-08-19T19:56:02.661036Z 01O src/registry/artifacts.rs:34:    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
2026-08-19T19:56:02.661039Z 01O src/sandbox/container.rs:541:        std::env::var_os("HOME").map(PathBuf::from),
2026-08-19T19:56:02.661042Z 01O src/sandbox/container.rs:542:        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
2026-08-19T19:56:02.661045Z 01O src/sandbox/container.rs:543:        std::env::var("UID").ok(),
2026-08-19T19:56:02.661048Z 01O src/sandbox/proxy/http.rs:58:        std::env::var(name).ok()
2026-08-19T19:56:02.661051Z 01O src/secrets/mod.rs:122:    if let Ok(env_key) = std::env::var("SECRETS_MASTER_KEY")
2026-08-19T19:56:02.661054Z 01O src/service.rs:637:    if let Ok(explicit) = std::env::var("XMPP_BRIDGE_EXECUTABLE") {
2026-08-19T19:56:02.661057Z 01O src/service.rs:945:    let user = std::env::var("SUDO_USER")
2026-08-19T19:56:02.661060Z 01O src/service.rs:947:        .or_else(|| std::env::var("USER").ok())
2026-08-19T19:56:02.661063Z 01O src/service.rs:969:    std::env::var_os("PATH")
2026-08-19T19:56:02.661066Z 01O src/skills/catalog.rs:165:        let registry_url = std::env::var("CLAWHUB_REGISTRY")
2026-08-19T19:56:02.661069Z 01O src/skills/catalog.rs:166:            .or_else(|_| std::env::var("CLAWDHUB_REGISTRY"))
2026-08-19T19:56:02.661086Z 01O src/skills/gating.rs:60:        if std::env::var(var).is_err() {
2026-08-19T19:56:02.661089Z 01O src/tools/builtin/shell.rs:688:            if let Ok(val) = std::env::var(var) {
2026-08-19T19:56:02.661092Z 01O src/tools/builtin/ssh_git.rs:461:        if let Ok(val) = std::env::var(var) {
2026-08-19T19:56:02.661095Z 01O src/tools/mcp/gateway_control.rs:367:            std::env::var(GATEWAY_CONTROL_BIND_ENV).ok(),
2026-08-19T19:56:02.661098Z 01O src/tools/mcp/gateway_control.rs:368:            std::env::var(GATEWAY_CONTROL_ADMIN_TOKEN_ENV).ok(),
2026-08-19T19:56:02.661102Z 01O src/tools/mcp/gateway_control.rs:369:            std::env::var(GATEWAY_CONTROL_ROUTES_PATH_ENV).ok(),
2026-08-19T19:56:02.661105Z 01O src/tools/mcp/gateway_control.rs:370:            std::env::var(GATEWAY_CONTROL_ALLOW_NON_LOOPBACK_ENV).ok(),
2026-08-19T19:56:02.661108Z 01O src/tools/mcp/provenance.rs:500:    let Some(path_var) = std::env::var_os("PATH") else {
2026-08-19T19:56:02.661111Z 01O src/tools/mcp/stdio_transport.rs:204:    std::env::vars_os()
2026-08-19T19:56:02.661114Z 01O src/tools/mcp/config.rs:1024:        parse_study_notes_allow_write(std::env::var_os(STUDY_NOTES_ALLOW_WRITE_ENV).as_deref())?;
2026-08-19T19:56:02.661118Z 01O src/tools/mcp/config.rs:1025:    let home = std::env::var_os(STUDY_NOTES_HOME_ENV);
2026-08-19T19:56:02.661121Z 01O src/tools/mcp/config.rs:1026:    let state_home = std::env::var_os(STUDY_NOTES_STATE_HOME_ENV);
2026-08-19T19:56:02.661124Z 01O src/tools/process.rs:132:    if let Ok(slice) = std::env::var("LUNARWING_WORKLOAD_SLICE")
2026-08-19T19:56:02.661128Z 01O src/tools/process.rs:137:    spawner.memory_max = std::env::var("LUNARWING_WORKLOAD_MEMORY_MAX")
2026-08-19T19:56:02.661131Z 01O src/tools/process.rs:167:    std::env::var("LUNARWING_WORKLOAD_ISOLATION")
2026-08-19T19:56:02.661134Z 01O src/tools/process.rs:181:        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR")
2026-08-19T19:56:02.661137Z 01O src/tools/wasm/loader.rs:475:                .and_then(|env| std::env::var(env).ok())
2026-08-19T19:56:02.661140Z 01O src/tools/wasm/loader.rs:486:                .and_then(|env| std::env::var(env).ok())
2026-08-19T19:56:02.661143Z 01O src/tools/wasm/loader.rs:572:        std::env::var("LUNARWING_TOOLS_SRC").or_else(|_| std::env::var("IRONCLAW_TOOLS_SRC"))
2026-08-19T19:56:02.661147Z 01O src/tools/wasm/loader.rs:814:        let previous = std::env::var(key).ok();
2026-08-19T19:56:02.661150Z 01O src/tools/wasm/wrapper.rs:542:        if std::env::var("ALLOW_PRIVATE_IPS").ok().as_deref() != Some("1") {
2026-08-19T19:56:02.661154Z 01O src/tunnel/netbird.rs:438:        let Ok(pid_path) = std::env::var("LUNARWING_PDEATHSIG_HELPER") else {
2026-08-19T19:56:02.661157Z 01O src/worker/api.rs:143:        let token = std::env::var("LUNARWING_WORKER_TOKEN")
2026-08-19T19:56:02.661160Z 01O src/worker/api.rs:144:            .or_else(|_| std::env::var("IRONCLAW_WORKER_TOKEN"))
2026-08-19T19:56:02.661163Z 01O src/worker/mod.rs:50:    let agent_command = std::env::var("ACP_AGENT_COMMAND").map_err(|_| {
2026-08-19T19:56:02.661167Z 01O src/worker/mod.rs:54:    let agent_args: Vec<String> = match std::env::var("ACP_AGENT_ARGS") {
2026-08-19T19:56:02.661170Z 01O src/worker/mod.rs:62:    let agent_env: std::collections::HashMap<String, String> = match std::env::var("ACP_AGENT_ENV")
2026-08-19T19:56:02.661174Z 01O 
2026-08-19T19:56:02.661176Z 01O (Review these -- secrets/config should come through Config or the secrets module)
2026-08-19T19:56:02.661179Z 01O 
2026-08-19T19:56:02.661181Z 01O --- Check 4: Test tier gating for integration tests ---
2026-08-19T19:56:02.665520Z 01O OK
2026-08-19T19:56:02.665527Z 01O 
2026-08-19T19:56:02.665529Z 01O --- Check 5: No silent test-skip patterns ---
2026-08-19T19:56:02.665531Z 01O OK
2026-08-19T19:56:02.665532Z 01O 
2026-08-19T19:56:02.665533Z 01O --- Check 6: LLM module isolation ---
2026-08-19T19:56:02.674569Z 01O WARNING: src/llm/ has 27 reference(s) to modules outside crate::llm:
2026-08-19T19:56:02.674579Z 01O src/llm/config.rs:159:        let api_key = crate::config::helpers::env_or_override("LUNARWING_CLOUD_API_KEY")
2026-08-19T19:56:02.674621Z 01O src/llm/config.rs:168:        let base_url = crate::config::helpers::env_or_override("LUNARWING_CLOUD_BASE_URL")
2026-08-19T19:56:02.674624Z 01O src/llm/lunarwing_cloud_chat.rs:310:            let truncated = crate::agent::truncate_for_preview(&response_text, 512);
2026-08-19T19:56:02.674626Z 01O src/llm/lunarwing_cloud_chat.rs:318:            let truncated = crate::agent::truncate_for_preview(&response_text, 512);
2026-08-19T19:56:02.674628Z 01O src/llm/lunarwing_cloud_chat.rs:370:            let truncated = crate::agent::truncate_for_preview(&response_text, 512);
2026-08-19T19:56:02.674630Z 01O src/llm/lunarwing_cloud_chat.rs:665:    fn set_model(&self, model: &str) -> Result<(), crate::error::LlmError> {
2026-08-19T19:56:02.674632Z 01O src/llm/models.rs:109:pub(crate) fn build_lunarwing_cloud_model_fetch_config() -> crate::config::LlmConfig {
2026-08-19T19:56:02.674634Z 01O src/llm/models.rs:110:    let auth_base_url = crate::config::helpers::env_or_override("LUNARWING_CLOUD_AUTH_URL")
2026-08-19T19:56:02.674636Z 01O src/llm/models.rs:113:    let lunarwing_cloud = crate::config::LunarWingCloudConfig::for_model_discovery();
2026-08-19T19:56:02.674638Z 01O src/llm/models.rs:114:    crate::config::LlmConfig {
2026-08-19T19:56:02.674640Z 01O src/llm/models.rs:118:            session_path: crate::config::llm::default_session_path(),
2026-08-19T19:56:02.674642Z 01O src/llm/oauth_helpers.rs:43:    crate::config::helpers::env_or_override("LUNARWING_OAUTH_CALLBACK_URL")
2026-08-19T19:56:02.674644Z 01O src/llm/oauth_helpers.rs:59:    crate::config::helpers::env_or_override("OAUTH_CALLBACK_HOST")
2026-08-19T19:56:02.674646Z 01O src/llm/oauth_helpers.rs:365:    use crate::config::helpers::lock_env;
2026-08-19T19:56:02.674647Z 01O src/llm/reasoning.rs:2001:        other => crate::channels::budget::budget_for_channel(other)
2026-08-19T19:56:02.674649Z 01O src/llm/recording.rs:616:    pub async fn snapshot_memory(&self, workspace: &crate::workspace::Workspace) {
2026-08-19T19:56:02.674651Z 01O src/llm/registry.rs:316:    Some(crate::bootstrap::lunarwing_base_dir().join("providers.json"))
2026-08-19T19:56:02.674653Z 01O src/llm/session.rs:58:    store: RwLock<Option<Arc<dyn crate::db::Database>>>,
2026-08-19T19:56:02.674655Z 01O src/llm/session.rs:121:    pub async fn attach_store(&self, store: Arc<dyn crate::db::Database>, user_id: &str) {
2026-08-19T19:56:02.674657Z 01O src/llm/session.rs:199:        let preview = crate::agent::truncate_for_preview(&body, 200);
2026-08-19T19:56:02.674659Z 01O src/llm/session.rs:398:            crate::setup::secret_input("API key").map_err(|e| LlmError::SessionRenewalFailed {
2026-08-19T19:56:02.674661Z 01O src/llm/session.rs:416:        crate::config::helpers::set_runtime_env("LUNARWING_CLOUD_API_KEY", &key);
2026-08-19T19:56:02.674663Z 01O src/llm/session.rs:421:        if let Err(e) = crate::bootstrap::upsert_bootstrap_var("LUNARWING_CLOUD_API_KEY", &key) {
2026-08-19T19:56:02.674665Z 01O src/llm/session.rs:426:        crate::setup::print_success("LunarWing Cloud Cloud API key saved.");
2026-08-19T19:56:02.674667Z 01O src/llm/transcription/mod.rs:103:    pub async fn process(&self, msg: &mut crate::channels::IncomingMessage) {
2026-08-19T19:56:02.674669Z 01O src/llm/transcription/mod.rs:104:        use crate::channels::AttachmentKind;
2026-08-19T19:56:02.674671Z 01O src/llm/transcription/mod.rs:169:    use crate::channels::{AttachmentKind, IncomingAttachment, IncomingMessage};
2026-08-19T19:56:02.674673Z 01O 
2026-08-19T19:56:02.674674Z 01O (These are pre-existing; fix them before extracting the crate.)
2026-08-19T19:56:02.674675Z 01O (New 'use crate::' imports are hard violations — see below.)
2026-08-19T19:56:02.674687Z 01O 
2026-08-19T19:56:02.682302Z 01O 
2026-08-19T19:56:02.682308Z 01O === Summary ===
2026-08-19T19:56:02.682309Z 01O FAILED: 1 hard violation(s) found
2026-08-19T19:56:02.932097Z 00O section_end:1787169362:step_script
[0K
2026-08-19T19:56:02.934036Z 00O+section_start:1787169362:cleanup_file_variables
[0K
2026-08-19T19:56:02.934098Z 00O+[0K[36;1mCleaning up project directory and file based variables[0;m[0;m
2026-08-19T19:56:03.474023Z 00O section_end:1787169363:cleanup_file_variables
[0K
2026-08-19T19:56:03.584037Z 00O [31;1mERROR: Job failed: exit code 1[0;m
