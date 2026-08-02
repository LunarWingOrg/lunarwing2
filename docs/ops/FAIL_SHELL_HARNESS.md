2026-08-02T04:56:16.430028Z 00O [0KRunning with gitlab-runner 19.2.0 (39acda30)[0;m
2026-08-02T04:56:16.430066Z 00O [0K  on lunarwing-lint-docker lYlWumyKW, system ID: r_dLo2MDLTpMm2[0;m
2026-08-02T04:56:16.430164Z 00O section_start:1785646576:prepare_executor
[0K
2026-08-02T04:56:16.430173Z 00O+[0K[36;1mPreparing the "docker" executor[0;m[0;m
2026-08-02T04:56:16.461402Z 00O [0KUsing Docker executor with image docker.io/library/python:3.12-slim-bookworm@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b ...[0;m
2026-08-02T04:56:17.525995Z 00O [0KUsing effective pull policy of [always] for container docker.io/library/python:3.12-slim-bookworm@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b[0;m
2026-08-02T04:56:17.527886Z 00O [0KPulling docker image docker.io/library/python:3.12-slim-bookworm@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b ...[0;m
2026-08-02T04:56:17.795516Z 00O [0KUsing docker image sha256:a1e028236d55ab8a2ec45d7c693137b5adb5d230baecdbae4f9f75cf51b6ce66 for docker.io/library/python:3.12-slim-bookworm@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b with digest python@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b ...[0;m
2026-08-02T04:56:17.795542Z 00O section_end:1785646577:prepare_executor
[0K
2026-08-02T04:56:17.795873Z 00O+section_start:1785646577:prepare_script
[0K
2026-08-02T04:56:17.795884Z 00O+[0K[36;1mPreparing environment[0;m[0;m
2026-08-02T04:56:17.797541Z 00O [0KUsing effective pull policy of [always] for container sha256:f9f935a39684abcbf90c964bcbf2d8da434697961edbc0e381cf2b4ddf996e21[0;m
2026-08-02T04:56:18.125443Z 01O Running on runner-lylwumykw-project-35-concurrent-0 via 389adee45b97...
2026-08-02T04:56:18.339967Z 00O section_end:1785646578:prepare_script
[0K
2026-08-02T04:56:18.341415Z 00O+section_start:1785646578:get_sources
[0K
2026-08-02T04:56:18.341429Z 00O+[0K[36;1mGetting source from Git repository[0;m[0;m
2026-08-02T04:56:18.765935Z 01O [32;1mGitaly correlation ID: 01KZ0D8W08EASNF1SRBZJTVKSF[0;m
2026-08-02T04:56:18.776173Z 01O [32;1mFetching changes...[0;m
2026-08-02T04:56:18.786230Z 01O Reinitialized existing Git repository in /builds/lunarwing/lunarwing2/.git/
2026-08-02T04:56:18.791156Z 01O [32;1mCreated fresh repository.[0;m
2026-08-02T04:56:19.221637Z 01O [32;1mChecking out 5b507966 as detached HEAD (ref is dwarfstar4/x/v2.0.4.0)...[0;m
2026-08-02T04:56:19.299549Z 01O Removing .ci-cache/
2026-08-02T04:56:19.461817Z 01O 
2026-08-02T04:56:19.461829Z 01O [32;1mSkipping Git submodules setup[0;m
2026-08-02T04:56:19.639502Z 00O section_end:1785646579:get_sources
[0K
2026-08-02T04:56:19.640981Z 00O+section_start:1785646579:restore_cache
[0K
2026-08-02T04:56:19.640995Z 00O+[0K[36;1mRestoring cache[0;m[0;m
2026-08-02T04:56:20.047712Z 01O [32;1mChecking cache for python-3.12-ee15b2df12c8bc0aaad9af877f1133223661083f-5-protected...[0;m
2026-08-02T04:56:20.183163Z 01E No URL provided, cache will not be downloaded from shared cache server. Instead a local version of cache will be extracted.[0;m 
2026-08-02T04:56:20.484640Z 01O [32;1mSuccessfully extracted cache[0;m
2026-08-02T04:56:20.674938Z 00O section_end:1785646580:restore_cache
[0K
2026-08-02T04:56:20.678018Z 00O+section_start:1785646580:step_script
[0K
2026-08-02T04:56:20.678054Z 00O+[0K[36;1mExecuting "step_script" stage of the job script[0;m[0;m
2026-08-02T04:56:20.678090Z 00O [0KUsing effective pull policy of [always] for container docker.io/library/python:3.12-slim-bookworm@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b[0;m
2026-08-02T04:56:20.680041Z 00O [0KUsing docker image sha256:a1e028236d55ab8a2ec45d7c693137b5adb5d230baecdbae4f9f75cf51b6ce66 for docker.io/library/python:3.12-slim-bookworm@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b with digest python@sha256:d50fb7611f86d04a3b0471b46d7557818d88983fc3136726336b2a4c657aa30b ...[0;m
2026-08-02T04:56:21.118857Z 01O [32;1m$ bash .gitlab/scripts/bootstrap-python.sh[0;m
2026-08-02T04:56:21.299752Z 01O Get:1 http://deb.debian.org/debian bookworm InRelease [151 kB]
2026-08-02T04:56:21.411429Z 01O Get:2 http://deb.debian.org/debian bookworm-updates InRelease [55.4 kB]
2026-08-02T04:56:21.451950Z 01O Get:3 http://deb.debian.org/debian-security bookworm-security InRelease [34.8 kB]
2026-08-02T04:56:21.615273Z 01O Get:4 http://deb.debian.org/debian bookworm/main amd64 Packages [8790 kB]
2026-08-02T04:56:21.921084Z 01O Get:5 http://deb.debian.org/debian bookworm-updates/main amd64 Packages [6924 B]
2026-08-02T04:56:21.923476Z 01O Get:6 http://deb.debian.org/debian-security bookworm-security/main amd64 Packages [321 kB]
2026-08-02T04:56:22.847419Z 01O Fetched 9360 kB in 2s (5671 kB/s)
2026-08-02T04:56:22.847431Z 01O Reading package lists...
2026-08-02T04:56:23.528661Z 01O Reading package lists...
2026-08-02T04:56:24.149105Z 01O Building dependency tree...
2026-08-02T04:56:24.351083Z 01O Reading state information...
2026-08-02T04:56:24.568838Z 01O bash is already the newest version (5.2.15-2+b13).
2026-08-02T04:56:24.568852Z 01O bash set to manually installed.
2026-08-02T04:56:24.568853Z 01O ca-certificates is already the newest version (20230311+deb12u1).
2026-08-02T04:56:24.568855Z 01O coreutils is already the newest version (9.1-1).
2026-08-02T04:56:24.568856Z 01O coreutils set to manually installed.
2026-08-02T04:56:24.568857Z 01O gzip is already the newest version (1.12-1).
2026-08-02T04:56:24.568858Z 01O gzip set to manually installed.
2026-08-02T04:56:24.568859Z 01O tar is already the newest version (1.34+dfsg-1.2+deb12u1).
2026-08-02T04:56:24.568860Z 01O tar set to manually installed.
2026-08-02T04:56:24.568861Z 01O util-linux is already the newest version (2.38.1-5+deb12u3).
2026-08-02T04:56:24.568862Z 01O util-linux set to manually installed.
2026-08-02T04:56:24.568863Z 01O The following additional packages will be installed:
2026-08-02T04:56:24.568872Z 01O   git-man libbrotli1 libcurl3-gnutls libcurl4 liberror-perl
2026-08-02T04:56:24.569116Z 01O   libevent-core-2.1-7 libexpat1 libgdbm-compat4 libjq1 libldap-2.5-0
2026-08-02T04:56:24.569880Z 01O   libnghttp2-14 libonig5 libperl5.36 libproc2-0 libpsl5 librtmp1 libsasl2-2
2026-08-02T04:56:24.571004Z 01O   libsasl2-modules-db libssh2-1 libutempter0 p7zip perl perl-modules-5.36
2026-08-02T04:56:24.573284Z 01O Suggested packages:
2026-08-02T04:56:24.573289Z 01O   gettext-base git-daemon-run | git-daemon-sysvinit git-doc git-email git-gui
2026-08-02T04:56:24.573332Z 01O   gitk gitweb git-cvs git-mediawiki git-svn sensible-utils p7zip-rar perl-doc
2026-08-02T04:56:24.573334Z 01O   libterm-readline-gnu-perl | libterm-readline-perl-perl make
2026-08-02T04:56:24.573335Z 01O   libtap-harness-archive-perl
2026-08-02T04:56:24.573336Z 01O Recommended packages:
2026-08-02T04:56:24.573337Z 01O   patch less ssh-client libldap-common publicsuffix libsasl2-modules psmisc
2026-08-02T04:56:24.765562Z 01O The following NEW packages will be installed:
2026-08-02T04:56:24.765587Z 01O   curl git git-man jq libbrotli1 libcurl3-gnutls libcurl4 liberror-perl
2026-08-02T04:56:24.765797Z 01O   libevent-core-2.1-7 libexpat1 libgdbm-compat4 libjq1 libldap-2.5-0
2026-08-02T04:56:24.766310Z 01O   libnghttp2-14 libonig5 libperl5.36 libproc2-0 libpsl5 librtmp1 libsasl2-2
2026-08-02T04:56:24.766456Z 01O   libsasl2-modules-db libssh2-1 libutempter0 p7zip p7zip-full perl
2026-08-02T04:56:24.766839Z 01O   perl-modules-5.36 procps shellcheck tmux
2026-08-02T04:56:24.890399Z 01O 0 upgraded, 30 newly installed, 0 to remove and 0 not upgraded.
2026-08-02T04:56:24.890412Z 01O Need to get 25.0 MB of archives.
2026-08-02T04:56:24.890414Z 01O After this operation, 133 MB of additional disk space will be used.
2026-08-02T04:56:24.890416Z 01O Get:1 http://deb.debian.org/debian bookworm/main amd64 perl-modules-5.36 all 5.36.0-7+deb12u3 [2815 kB]
2026-08-02T04:56:25.167126Z 01O Get:2 http://deb.debian.org/debian bookworm/main amd64 libgdbm-compat4 amd64 1.23-3 [48.2 kB]
2026-08-02T04:56:25.168186Z 01O Get:3 http://deb.debian.org/debian bookworm/main amd64 libperl5.36 amd64 5.36.0-7+deb12u3 [4196 kB]
2026-08-02T04:56:25.226332Z 01O Get:4 http://deb.debian.org/debian bookworm/main amd64 perl amd64 5.36.0-7+deb12u3 [239 kB]
2026-08-02T04:56:25.228287Z 01O Get:5 http://deb.debian.org/debian bookworm/main amd64 libproc2-0 amd64 2:4.0.2-3 [62.8 kB]
2026-08-02T04:56:25.258993Z 01O Get:6 http://deb.debian.org/debian bookworm/main amd64 procps amd64 2:4.0.2-3 [709 kB]
2026-08-02T04:56:25.265346Z 01O Get:7 http://deb.debian.org/debian bookworm/main amd64 libbrotli1 amd64 1.0.9-2+b6 [275 kB]
2026-08-02T04:56:25.267750Z 01O Get:8 http://deb.debian.org/debian bookworm/main amd64 libsasl2-modules-db amd64 2.1.28+dfsg-10 [20.3 kB]
2026-08-02T04:56:25.267943Z 01O Get:9 http://deb.debian.org/debian bookworm/main amd64 libsasl2-2 amd64 2.1.28+dfsg-10 [59.7 kB]
2026-08-02T04:56:25.299828Z 01O Get:10 http://deb.debian.org/debian bookworm/main amd64 libldap-2.5-0 amd64 2.5.13+dfsg-5 [183 kB]
2026-08-02T04:56:25.301620Z 01O Get:11 http://deb.debian.org/debian bookworm/main amd64 libnghttp2-14 amd64 1.52.0-1+deb12u3 [72.4 kB]
2026-08-02T04:56:25.302300Z 01O Get:12 http://deb.debian.org/debian bookworm/main amd64 libpsl5 amd64 0.21.2-1 [58.7 kB]
2026-08-02T04:56:25.303018Z 01O Get:13 http://deb.debian.org/debian bookworm/main amd64 librtmp1 amd64 2.4+20151223.gitfa8646d.1-2+b2 [60.8 kB]
2026-08-02T04:56:25.303843Z 01O Get:14 http://deb.debian.org/debian bookworm/main amd64 libssh2-1 amd64 1.10.0-3+b1 [179 kB]
2026-08-02T04:56:25.305826Z 01O Get:15 http://deb.debian.org/debian bookworm/main amd64 libcurl4 amd64 7.88.1-10+deb12u15 [392 kB]
2026-08-02T04:56:25.311961Z 01O Get:16 http://deb.debian.org/debian bookworm/main amd64 curl amd64 7.88.1-10+deb12u15 [316 kB]
2026-08-02T04:56:25.314951Z 01O Get:17 http://deb.debian.org/debian bookworm/main amd64 libcurl3-gnutls amd64 7.88.1-10+deb12u15 [386 kB]
2026-08-02T04:56:25.318263Z 01O Get:18 http://deb.debian.org/debian bookworm/main amd64 libexpat1 amd64 2.5.0-1+deb12u2 [99.9 kB]
2026-08-02T04:56:25.319070Z 01O Get:19 http://deb.debian.org/debian bookworm/main amd64 liberror-perl all 0.17029-2 [29.0 kB]
2026-08-02T04:56:25.353790Z 01O Get:20 http://deb.debian.org/debian bookworm/main amd64 git-man all 1:2.39.5-0+deb12u3 [2053 kB]
2026-08-02T04:56:25.398790Z 01O Get:21 http://deb.debian.org/debian bookworm/main amd64 git amd64 1:2.39.5-0+deb12u3 [7264 kB]
2026-08-02T04:56:25.544104Z 01O Get:22 http://deb.debian.org/debian bookworm/main amd64 libonig5 amd64 6.9.8-1 [188 kB]
2026-08-02T04:56:25.544647Z 01O Get:23 http://deb.debian.org/debian bookworm/main amd64 libjq1 amd64 1.6-2.1+deb12u2 [136 kB]
2026-08-02T04:56:25.545457Z 01O Get:24 http://deb.debian.org/debian bookworm/main amd64 jq amd64 1.6-2.1+deb12u2 [64.0 kB]
2026-08-02T04:56:25.545922Z 01O Get:25 http://deb.debian.org/debian bookworm/main amd64 libevent-core-2.1-7 amd64 2.1.12-stable-8 [131 kB]
2026-08-02T04:56:25.546519Z 01O Get:26 http://deb.debian.org/debian bookworm/main amd64 libutempter0 amd64 1.2.1-3 [8960 B]
2026-08-02T04:56:25.546704Z 01O Get:27 http://deb.debian.org/debian bookworm/main amd64 p7zip amd64 16.02+really26.01+dfsg-0+deb12u1 [453 kB]
2026-08-02T04:56:25.572884Z 01O Get:28 http://deb.debian.org/debian bookworm/main amd64 p7zip-full amd64 16.02+really26.01+dfsg-0+deb12u1 [1423 kB]
2026-08-02T04:56:25.587602Z 01O Get:29 http://deb.debian.org/debian bookworm/main amd64 shellcheck amd64 0.9.0-1 [2621 kB]
2026-08-02T04:56:25.619432Z 01O Get:30 http://deb.debian.org/debian bookworm/main amd64 tmux amd64 3.3a-3 [455 kB]
2026-08-02T04:56:25.889268Z 01E debconf: delaying package configuration, since apt-utils is not installed
2026-08-02T04:56:25.963587Z 01O Fetched 25.0 MB in 1s (29.4 MB/s)
2026-08-02T04:56:26.004853Z 01O Selecting previously unselected package perl-modules-5.36.
2026-08-02T04:56:26.004874Z 01O (Reading database ... 
(Reading database ... 5%
2026-08-02T04:56:26.004890Z 01O+(Reading database ... 10%
(Reading database ... 15%
(Reading database ... 20%
(Reading database ... 25%
2026-08-02T04:56:26.004922Z 01O+(Reading database ... 30%
(Reading database ... 35%
(Reading database ... 40%
2026-08-02T04:56:26.005012Z 01O+(Reading database ... 45%
(Reading database ... 50%
(Reading database ... 55%
(Reading database ... 60%
2026-08-02T04:56:26.005084Z 01O+(Reading database ... 65%
(Reading database ... 70%
2026-08-02T04:56:26.007302Z 01O+(Reading database ... 75%
2026-08-02T04:56:26.016697Z 01O+(Reading database ... 80%
2026-08-02T04:56:26.020432Z 01O+(Reading database ... 85%
2026-08-02T04:56:26.024066Z 01O+(Reading database ... 90%
2026-08-02T04:56:26.030425Z 01O+(Reading database ... 95%
2026-08-02T04:56:26.036698Z 01O+(Reading database ... 100%
(Reading database ... 6695 files and directories currently installed.)
2026-08-02T04:56:26.038028Z 01O Preparing to unpack .../00-perl-modules-5.36_5.36.0-7+deb12u3_all.deb ...
2026-08-02T04:56:26.039399Z 01O Unpacking perl-modules-5.36 (5.36.0-7+deb12u3) ...
2026-08-02T04:56:26.327527Z 01O Selecting previously unselected package libgdbm-compat4:amd64.
2026-08-02T04:56:26.328434Z 01O Preparing to unpack .../01-libgdbm-compat4_1.23-3_amd64.deb ...
2026-08-02T04:56:26.330511Z 01O Unpacking libgdbm-compat4:amd64 (1.23-3) ...
2026-08-02T04:56:26.366476Z 01O Selecting previously unselected package libperl5.36:amd64.
2026-08-02T04:56:26.367679Z 01O Preparing to unpack .../02-libperl5.36_5.36.0-7+deb12u3_amd64.deb ...
2026-08-02T04:56:26.368769Z 01O Unpacking libperl5.36:amd64 (5.36.0-7+deb12u3) ...
2026-08-02T04:56:26.855859Z 01O Selecting previously unselected package perl.
2026-08-02T04:56:26.858810Z 01O Preparing to unpack .../03-perl_5.36.0-7+deb12u3_amd64.deb ...
2026-08-02T04:56:26.878244Z 01O Unpacking perl (5.36.0-7+deb12u3) ...
2026-08-02T04:56:26.960058Z 01O Selecting previously unselected package libproc2-0:amd64.
2026-08-02T04:56:26.962610Z 01O Preparing to unpack .../04-libproc2-0_2%3a4.0.2-3_amd64.deb ...
2026-08-02T04:56:26.964690Z 01O Unpacking libproc2-0:amd64 (2:4.0.2-3) ...
2026-08-02T04:56:27.000832Z 01O Selecting previously unselected package procps.
2026-08-02T04:56:27.003054Z 01O Preparing to unpack .../05-procps_2%3a4.0.2-3_amd64.deb ...
2026-08-02T04:56:27.026588Z 01O Unpacking procps (2:4.0.2-3) ...
2026-08-02T04:56:27.160337Z 01O Selecting previously unselected package libbrotli1:amd64.
2026-08-02T04:56:27.162951Z 01O Preparing to unpack .../06-libbrotli1_1.0.9-2+b6_amd64.deb ...
2026-08-02T04:56:27.164578Z 01O Unpacking libbrotli1:amd64 (1.0.9-2+b6) ...
2026-08-02T04:56:27.208875Z 01O Selecting previously unselected package libsasl2-modules-db:amd64.
2026-08-02T04:56:27.211355Z 01O Preparing to unpack .../07-libsasl2-modules-db_2.1.28+dfsg-10_amd64.deb ...
2026-08-02T04:56:27.212724Z 01O Unpacking libsasl2-modules-db:amd64 (2.1.28+dfsg-10) ...
2026-08-02T04:56:27.237618Z 01O Selecting previously unselected package libsasl2-2:amd64.
2026-08-02T04:56:27.240090Z 01O Preparing to unpack .../08-libsasl2-2_2.1.28+dfsg-10_amd64.deb ...
2026-08-02T04:56:27.241597Z 01O Unpacking libsasl2-2:amd64 (2.1.28+dfsg-10) ...
2026-08-02T04:56:27.270846Z 01O Selecting previously unselected package libldap-2.5-0:amd64.
2026-08-02T04:56:27.272272Z 01O Preparing to unpack .../09-libldap-2.5-0_2.5.13+dfsg-5_amd64.deb ...
2026-08-02T04:56:27.273041Z 01O Unpacking libldap-2.5-0:amd64 (2.5.13+dfsg-5) ...
2026-08-02T04:56:27.308509Z 01O Selecting previously unselected package libnghttp2-14:amd64.
2026-08-02T04:56:27.310183Z 01O Preparing to unpack .../10-libnghttp2-14_1.52.0-1+deb12u3_amd64.deb ...
2026-08-02T04:56:27.311132Z 01O Unpacking libnghttp2-14:amd64 (1.52.0-1+deb12u3) ...
2026-08-02T04:56:27.332468Z 01O Selecting previously unselected package libpsl5:amd64.
2026-08-02T04:56:27.333676Z 01O Preparing to unpack .../11-libpsl5_0.21.2-1_amd64.deb ...
2026-08-02T04:56:27.334451Z 01O Unpacking libpsl5:amd64 (0.21.2-1) ...
2026-08-02T04:56:27.355697Z 01O Selecting previously unselected package librtmp1:amd64.
2026-08-02T04:56:27.355708Z 01O Preparing to unpack .../12-librtmp1_2.4+20151223.gitfa8646d.1-2+b2_amd64.deb ...
2026-08-02T04:56:27.356303Z 01O Unpacking librtmp1:amd64 (2.4+20151223.gitfa8646d.1-2+b2) ...
2026-08-02T04:56:27.379620Z 01O Selecting previously unselected package libssh2-1:amd64.
2026-08-02T04:56:27.379631Z 01O Preparing to unpack .../13-libssh2-1_1.10.0-3+b1_amd64.deb ...
2026-08-02T04:56:27.380448Z 01O Unpacking libssh2-1:amd64 (1.10.0-3+b1) ...
2026-08-02T04:56:27.411035Z 01O Selecting previously unselected package libcurl4:amd64.
2026-08-02T04:56:27.411044Z 01O Preparing to unpack .../14-libcurl4_7.88.1-10+deb12u15_amd64.deb ...
2026-08-02T04:56:27.411461Z 01O Unpacking libcurl4:amd64 (7.88.1-10+deb12u15) ...
2026-08-02T04:56:27.450290Z 01O Selecting previously unselected package curl.
2026-08-02T04:56:27.451333Z 01O Preparing to unpack .../15-curl_7.88.1-10+deb12u15_amd64.deb ...
2026-08-02T04:56:27.452009Z 01O Unpacking curl (7.88.1-10+deb12u15) ...
2026-08-02T04:56:27.491282Z 01O Selecting previously unselected package libcurl3-gnutls:amd64.
2026-08-02T04:56:27.493037Z 01O Preparing to unpack .../16-libcurl3-gnutls_7.88.1-10+deb12u15_amd64.deb ...
2026-08-02T04:56:27.494907Z 01O Unpacking libcurl3-gnutls:amd64 (7.88.1-10+deb12u15) ...
2026-08-02T04:56:27.590849Z 01O Selecting previously unselected package libexpat1:amd64.
2026-08-02T04:56:27.590859Z 01O Preparing to unpack .../17-libexpat1_2.5.0-1+deb12u2_amd64.deb ...
2026-08-02T04:56:27.592352Z 01O Unpacking libexpat1:amd64 (2.5.0-1+deb12u2) ...
2026-08-02T04:56:27.645593Z 01O Selecting previously unselected package liberror-perl.
2026-08-02T04:56:27.647837Z 01O Preparing to unpack .../18-liberror-perl_0.17029-2_all.deb ...
2026-08-02T04:56:27.650212Z 01O Unpacking liberror-perl (0.17029-2) ...
2026-08-02T04:56:27.689051Z 01O Selecting previously unselected package git-man.
2026-08-02T04:56:27.689063Z 01O Preparing to unpack .../19-git-man_1%3a2.39.5-0+deb12u3_all.deb ...
2026-08-02T04:56:27.691009Z 01O Unpacking git-man (1:2.39.5-0+deb12u3) ...
2026-08-02T04:56:27.851578Z 01O Selecting previously unselected package git.
2026-08-02T04:56:27.854653Z 01O Preparing to unpack .../20-git_1%3a2.39.5-0+deb12u3_amd64.deb ...
2026-08-02T04:56:27.868976Z 01O Unpacking git (1:2.39.5-0+deb12u3) ...
2026-08-02T04:56:28.295686Z 01O Selecting previously unselected package libonig5:amd64.
2026-08-02T04:56:28.295706Z 01O Preparing to unpack .../21-libonig5_6.9.8-1_amd64.deb ...
2026-08-02T04:56:28.297361Z 01O Unpacking libonig5:amd64 (6.9.8-1) ...
2026-08-02T04:56:28.371926Z 01O Selecting previously unselected package libjq1:amd64.
2026-08-02T04:56:28.374436Z 01O Preparing to unpack .../22-libjq1_1.6-2.1+deb12u2_amd64.deb ...
2026-08-02T04:56:28.376348Z 01O Unpacking libjq1:amd64 (1.6-2.1+deb12u2) ...
2026-08-02T04:56:28.435445Z 01O Selecting previously unselected package jq.
2026-08-02T04:56:28.437898Z 01O Preparing to unpack .../23-jq_1.6-2.1+deb12u2_amd64.deb ...
2026-08-02T04:56:28.439626Z 01O Unpacking jq (1.6-2.1+deb12u2) ...
2026-08-02T04:56:28.483698Z 01O Selecting previously unselected package libevent-core-2.1-7:amd64.
2026-08-02T04:56:28.484268Z 01O Preparing to unpack .../24-libevent-core-2.1-7_2.1.12-stable-8_amd64.deb ...
2026-08-02T04:56:28.486959Z 01O Unpacking libevent-core-2.1-7:amd64 (2.1.12-stable-8) ...
2026-08-02T04:56:28.546130Z 01O Selecting previously unselected package libutempter0:amd64.
2026-08-02T04:56:28.548879Z 01O Preparing to unpack .../25-libutempter0_1.2.1-3_amd64.deb ...
2026-08-02T04:56:28.550536Z 01O Unpacking libutempter0:amd64 (1.2.1-3) ...
2026-08-02T04:56:28.583755Z 01O Selecting previously unselected package p7zip.
2026-08-02T04:56:28.586451Z 01O Preparing to unpack .../26-p7zip_16.02+really26.01+dfsg-0+deb12u1_amd64.deb ...
2026-08-02T04:56:28.588888Z 01O Unpacking p7zip (16.02+really26.01+dfsg-0+deb12u1) ...
2026-08-02T04:56:28.657730Z 01O Selecting previously unselected package p7zip-full.
2026-08-02T04:56:28.660440Z 01O Preparing to unpack .../27-p7zip-full_16.02+really26.01+dfsg-0+deb12u1_amd64.deb ...
2026-08-02T04:56:28.673322Z 01O Unpacking p7zip-full (16.02+really26.01+dfsg-0+deb12u1) ...
2026-08-02T04:56:28.885107Z 01O Selecting previously unselected package shellcheck.
2026-08-02T04:56:28.887740Z 01O Preparing to unpack .../28-shellcheck_0.9.0-1_amd64.deb ...
2026-08-02T04:56:28.889504Z 01O Unpacking shellcheck (0.9.0-1) ...
2026-08-02T04:56:29.163803Z 01O Selecting previously unselected package tmux.
2026-08-02T04:56:29.164503Z 01O Preparing to unpack .../29-tmux_3.3a-3_amd64.deb ...
2026-08-02T04:56:29.169872Z 01O Unpacking tmux (3.3a-3) ...
2026-08-02T04:56:29.295772Z 01O Setting up libexpat1:amd64 (2.5.0-1+deb12u2) ...
2026-08-02T04:56:29.301465Z 01O Setting up libpsl5:amd64 (0.21.2-1) ...
2026-08-02T04:56:29.305579Z 01O Setting up libbrotli1:amd64 (1.0.9-2+b6) ...
2026-08-02T04:56:29.309404Z 01O Setting up libnghttp2-14:amd64 (1.52.0-1+deb12u3) ...
2026-08-02T04:56:29.313965Z 01O Setting up shellcheck (0.9.0-1) ...
2026-08-02T04:56:29.317911Z 01O Setting up libsasl2-modules-db:amd64 (2.1.28+dfsg-10) ...
2026-08-02T04:56:29.322760Z 01O Setting up perl-modules-5.36 (5.36.0-7+deb12u3) ...
2026-08-02T04:56:29.327283Z 01O Setting up librtmp1:amd64 (2.4+20151223.gitfa8646d.1-2+b2) ...
2026-08-02T04:56:29.332882Z 01O Setting up libproc2-0:amd64 (2:4.0.2-3) ...
2026-08-02T04:56:29.337180Z 01O Setting up libevent-core-2.1-7:amd64 (2.1.12-stable-8) ...
2026-08-02T04:56:29.341095Z 01O Setting up libgdbm-compat4:amd64 (1.23-3) ...
2026-08-02T04:56:29.344560Z 01O Setting up libsasl2-2:amd64 (2.1.28+dfsg-10) ...
2026-08-02T04:56:29.347865Z 01O Setting up libutempter0:amd64 (1.2.1-3) ...
2026-08-02T04:56:29.351458Z 01O Setting up procps (2:4.0.2-3) ...
2026-08-02T04:56:29.413837Z 01O Setting up git-man (1:2.39.5-0+deb12u3) ...
2026-08-02T04:56:29.416962Z 01O Setting up libssh2-1:amd64 (1.10.0-3+b1) ...
2026-08-02T04:56:29.420171Z 01O Setting up tmux (3.3a-3) ...
2026-08-02T04:56:29.459833Z 01O Setting up libperl5.36:amd64 (5.36.0-7+deb12u3) ...
2026-08-02T04:56:29.462959Z 01O Setting up libonig5:amd64 (6.9.8-1) ...
2026-08-02T04:56:29.466400Z 01O Setting up p7zip (16.02+really26.01+dfsg-0+deb12u1) ...
2026-08-02T04:56:29.469563Z 01O Setting up libjq1:amd64 (1.6-2.1+deb12u2) ...
2026-08-02T04:56:29.472851Z 01O Setting up p7zip-full (16.02+really26.01+dfsg-0+deb12u1) ...
2026-08-02T04:56:29.490238Z 01O Setting up libldap-2.5-0:amd64 (2.5.13+dfsg-5) ...
2026-08-02T04:56:29.493329Z 01O Setting up perl (5.36.0-7+deb12u3) ...
2026-08-02T04:56:29.517066Z 01O Setting up jq (1.6-2.1+deb12u2) ...
2026-08-02T04:56:29.520188Z 01O Setting up libcurl4:amd64 (7.88.1-10+deb12u15) ...
2026-08-02T04:56:29.523586Z 01O Setting up curl (7.88.1-10+deb12u15) ...
2026-08-02T04:56:29.527318Z 01O Setting up libcurl3-gnutls:amd64 (7.88.1-10+deb12u15) ...
2026-08-02T04:56:29.530572Z 01O Setting up liberror-perl (0.17029-2) ...
2026-08-02T04:56:29.533803Z 01O Setting up git (1:2.39.5-0+deb12u3) ...
2026-08-02T04:56:29.551025Z 01O Processing triggers for libc-bin (2.36-9+deb12u14) ...
2026-08-02T04:56:31.556836Z 01O Collecting annotated-doc==0.0.4 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 2))
2026-08-02T04:56:31.559694Z 01O   Using cached annotated_doc-0.0.4-py3-none-any.whl.metadata (6.6 kB)
2026-08-02T04:56:31.610812Z 01O Collecting annotated-types==0.7.0 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 3))
2026-08-02T04:56:31.613157Z 01O   Using cached annotated_types-0.7.0-py3-none-any.whl.metadata (15 kB)
2026-08-02T04:56:31.676112Z 01O Collecting anyio==4.14.2 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 4))
2026-08-02T04:56:31.678297Z 01O   Using cached anyio-4.14.2-py3-none-any.whl.metadata (4.6 kB)
2026-08-02T04:56:31.752705Z 01O Collecting certifi==2026.7.22 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 5))
2026-08-02T04:56:31.755430Z 01O   Using cached certifi-2026.7.22-py3-none-any.whl.metadata (2.5 kB)
2026-08-02T04:56:31.919051Z 01O Collecting cffi==2.1.0 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 6))
2026-08-02T04:56:31.920868Z 01O   Using cached cffi-2.1.0-cp312-cp312-manylinux2014_x86_64.manylinux_2_17_x86_64.whl.metadata (2.5 kB)
2026-08-02T04:56:31.982251Z 01O Collecting click==8.4.2 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 7))
2026-08-02T04:56:31.985131Z 01O   Using cached click-8.4.2-py3-none-any.whl.metadata (2.6 kB)
2026-08-02T04:56:32.249971Z 01O Collecting cryptography==49.0.0 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 8))
2026-08-02T04:56:32.251527Z 01O   Using cached cryptography-49.0.0-cp311-abi3-manylinux_2_34_x86_64.whl.metadata (4.3 kB)
2026-08-02T04:56:32.357885Z 01O Collecting fastapi==0.139.2 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 9))
2026-08-02T04:56:32.360104Z 01O   Using cached fastapi-0.139.2-py3-none-any.whl.metadata (26 kB)
2026-08-02T04:56:32.418026Z 01O Collecting h11==0.16.0 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 10))
2026-08-02T04:56:32.421519Z 01O   Using cached h11-0.16.0-py3-none-any.whl.metadata (8.3 kB)
2026-08-02T04:56:32.491687Z 01O Collecting httpcore==1.0.9 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 11))
2026-08-02T04:56:32.495559Z 01O   Using cached httpcore-1.0.9-py3-none-any.whl.metadata (21 kB)
2026-08-02T04:56:32.574088Z 01O Collecting httpx==0.28.1 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 12))
2026-08-02T04:56:32.577704Z 01O   Using cached httpx-0.28.1-py3-none-any.whl.metadata (7.1 kB)
2026-08-02T04:56:32.645140Z 01O Collecting idna==3.18 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 13))
2026-08-02T04:56:32.649111Z 01O   Using cached idna-3.18-py3-none-any.whl.metadata (6.1 kB)
2026-08-02T04:56:32.715520Z 01O Collecting markdown-it-py==4.2.0 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 14))
2026-08-02T04:56:32.719740Z 01O   Using cached markdown_it_py-4.2.0-py3-none-any.whl.metadata (7.4 kB)
2026-08-02T04:56:32.787617Z 01O Collecting mdurl==0.1.2 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 15))
2026-08-02T04:56:32.787632Z 01O   Using cached mdurl-0.1.2-py3-none-any.whl.metadata (1.6 kB)
2026-08-02T04:56:32.876492Z 01O Collecting prompt_toolkit==3.0.52 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 16))
2026-08-02T04:56:32.878970Z 01O   Using cached prompt_toolkit-3.0.52-py3-none-any.whl.metadata (6.4 kB)
2026-08-02T04:56:33.016286Z 01O Collecting psycopg2-binary==2.9.12 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 17))
2026-08-02T04:56:33.018213Z 01O   Using cached psycopg2_binary-2.9.12-cp312-cp312-manylinux2014_x86_64.manylinux_2_17_x86_64.whl.metadata (4.9 kB)
2026-08-02T04:56:33.067400Z 01O Collecting pycparser==3.0 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 18))
2026-08-02T04:56:33.070051Z 01O   Using cached pycparser-3.0-py3-none-any.whl.metadata (8.2 kB)
2026-08-02T04:56:33.240221Z 01O Collecting pydantic==2.13.4 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 19))
2026-08-02T04:56:33.241775Z 01O   Using cached pydantic-2.13.4-py3-none-any.whl.metadata (109 kB)
2026-08-02T04:56:33.908952Z 01O Collecting pydantic-core==2.46.4 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 20))
2026-08-02T04:56:33.910772Z 01O   Using cached pydantic_core-2.46.4-cp312-cp312-manylinux_2_17_x86_64.manylinux2014_x86_64.whl.metadata (6.6 kB)
2026-08-02T04:56:33.961450Z 01O Collecting pygments==2.20.0 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 21))
2026-08-02T04:56:33.962705Z 01O   Using cached pygments-2.20.0-py3-none-any.whl.metadata (2.5 kB)
2026-08-02T04:56:34.045686Z 01O Collecting PyYAML==6.0.3 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 22))
2026-08-02T04:56:34.047420Z 01O   Using cached pyyaml-6.0.3-cp312-cp312-manylinux2014_x86_64.manylinux_2_17_x86_64.manylinux_2_28_x86_64.whl.metadata (2.4 kB)
2026-08-02T04:56:34.101904Z 01O Collecting questionary==2.1.1 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 23))
2026-08-02T04:56:34.105564Z 01O   Using cached questionary-2.1.1-py3-none-any.whl.metadata (5.4 kB)
2026-08-02T04:56:34.181773Z 01O Collecting rich==15.0.0 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 24))
2026-08-02T04:56:34.183503Z 01O   Using cached rich-15.0.0-py3-none-any.whl.metadata (18 kB)
2026-08-02T04:56:34.247733Z 01O Collecting starlette==1.3.1 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 25))
2026-08-02T04:56:34.250051Z 01O   Using cached starlette-1.3.1-py3-none-any.whl.metadata (6.4 kB)
2026-08-02T04:56:34.313524Z 01O Collecting typing-extensions==4.16.0 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 26))
2026-08-02T04:56:34.316180Z 01O   Using cached typing_extensions-4.16.0-py3-none-any.whl.metadata (3.3 kB)
2026-08-02T04:56:34.371041Z 01O Collecting typing-inspection==0.4.2 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 27))
2026-08-02T04:56:34.375306Z 01O   Using cached typing_inspection-0.4.2-py3-none-any.whl.metadata (2.6 kB)
2026-08-02T04:56:34.466016Z 01O Collecting uvicorn==0.51.0 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 28))
2026-08-02T04:56:34.469697Z 01O   Using cached uvicorn-0.51.0-py3-none-any.whl.metadata (6.6 kB)
2026-08-02T04:56:34.540211Z 01O Collecting wcwidth==0.8.2 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 29))
2026-08-02T04:56:34.544355Z 01O   Using cached wcwidth-0.8.2-py3-none-any.whl.metadata (43 kB)
2026-08-02T04:56:34.753729Z 01O Collecting websockets==16.1.1 (from -r /builds/lunarwing/lunarwing2/.gitlab/requirements/python-tests.txt (line 30))
2026-08-02T04:56:34.755136Z 01O   Using cached websockets-16.1.1-cp312-cp312-manylinux1_x86_64.manylinux_2_28_x86_64.manylinux_2_5_x86_64.whl.metadata (6.8 kB)
2026-08-02T04:56:34.823064Z 01O Using cached annotated_doc-0.0.4-py3-none-any.whl (5.3 kB)
2026-08-02T04:56:34.825624Z 01O Using cached annotated_types-0.7.0-py3-none-any.whl (13 kB)
2026-08-02T04:56:34.827387Z 01O Using cached anyio-4.14.2-py3-none-any.whl (125 kB)
2026-08-02T04:56:34.829499Z 01O Using cached certifi-2026.7.22-py3-none-any.whl (136 kB)
2026-08-02T04:56:34.831815Z 01O Using cached cffi-2.1.0-cp312-cp312-manylinux2014_x86_64.manylinux_2_17_x86_64.whl (221 kB)
2026-08-02T04:56:34.833890Z 01O Using cached click-8.4.2-py3-none-any.whl (119 kB)
2026-08-02T04:56:34.835655Z 01O Using cached cryptography-49.0.0-cp311-abi3-manylinux_2_34_x86_64.whl (4.7 MB)
2026-08-02T04:56:34.840229Z 01O Using cached fastapi-0.139.2-py3-none-any.whl (130 kB)
2026-08-02T04:56:34.842198Z 01O Using cached h11-0.16.0-py3-none-any.whl (37 kB)
2026-08-02T04:56:34.844251Z 01O Using cached httpcore-1.0.9-py3-none-any.whl (78 kB)
2026-08-02T04:56:34.846021Z 01O Using cached httpx-0.28.1-py3-none-any.whl (73 kB)
2026-08-02T04:56:34.847690Z 01O Using cached idna-3.18-py3-none-any.whl (65 kB)
2026-08-02T04:56:34.849196Z 01O Using cached markdown_it_py-4.2.0-py3-none-any.whl (91 kB)
2026-08-02T04:56:34.850995Z 01O Using cached mdurl-0.1.2-py3-none-any.whl (10.0 kB)
2026-08-02T04:56:34.852440Z 01O Using cached prompt_toolkit-3.0.52-py3-none-any.whl (391 kB)
2026-08-02T04:56:34.854448Z 01O Using cached psycopg2_binary-2.9.12-cp312-cp312-manylinux2014_x86_64.manylinux_2_17_x86_64.whl (4.3 MB)
2026-08-02T04:56:34.858204Z 01O Using cached pycparser-3.0-py3-none-any.whl (48 kB)
2026-08-02T04:56:34.860276Z 01O Using cached pydantic-2.13.4-py3-none-any.whl (472 kB)
2026-08-02T04:56:34.863095Z 01O Using cached pydantic_core-2.46.4-cp312-cp312-manylinux_2_17_x86_64.manylinux2014_x86_64.whl (2.1 MB)
2026-08-02T04:56:34.868013Z 01O Using cached pygments-2.20.0-py3-none-any.whl (1.2 MB)
2026-08-02T04:56:34.872169Z 01O Using cached pyyaml-6.0.3-cp312-cp312-manylinux2014_x86_64.manylinux_2_17_x86_64.manylinux_2_28_x86_64.whl (807 kB)
2026-08-02T04:56:34.876362Z 01O Using cached questionary-2.1.1-py3-none-any.whl (36 kB)
2026-08-02T04:56:34.878825Z 01O Using cached rich-15.0.0-py3-none-any.whl (310 kB)
2026-08-02T04:56:34.881702Z 01O Using cached starlette-1.3.1-py3-none-any.whl (73 kB)
2026-08-02T04:56:34.884006Z 01O Using cached typing_extensions-4.16.0-py3-none-any.whl (45 kB)
2026-08-02T04:56:34.885698Z 01O Using cached typing_inspection-0.4.2-py3-none-any.whl (14 kB)
2026-08-02T04:56:34.886960Z 01O Using cached uvicorn-0.51.0-py3-none-any.whl (73 kB)
2026-08-02T04:56:34.888400Z 01O Using cached wcwidth-0.8.2-py3-none-any.whl (323 kB)
2026-08-02T04:56:34.890589Z 01O Using cached websockets-16.1.1-cp312-cp312-manylinux1_x86_64.manylinux_2_28_x86_64.manylinux_2_5_x86_64.whl (187 kB)
2026-08-02T04:56:35.012278Z 01O Installing collected packages: websockets, wcwidth, typing-extensions, PyYAML, pygments, pycparser, psycopg2-binary, mdurl, idna, h11, click, certifi, annotated-types, annotated-doc, uvicorn, typing-inspection, pydantic-core, prompt_toolkit, markdown-it-py, httpcore, cffi, anyio, starlette, rich, questionary, pydantic, httpx, cryptography, fastapi
2026-08-02T04:56:38.532414Z 01O Successfully installed PyYAML-6.0.3 annotated-doc-0.0.4 annotated-types-0.7.0 anyio-4.14.2 certifi-2026.7.22 cffi-2.1.0 click-8.4.2 cryptography-49.0.0 fastapi-0.139.2 h11-0.16.0 httpcore-1.0.9 httpx-0.28.1 idna-3.18 markdown-it-py-4.2.0 mdurl-0.1.2 prompt_toolkit-3.0.52 psycopg2-binary-2.9.12 pycparser-3.0 pydantic-2.13.4 pydantic-core-2.46.4 pygments-2.20.0 questionary-2.1.1 rich-15.0.0 starlette-1.3.1 typing-extensions-4.16.0 typing-inspection-0.4.2 uvicorn-0.51.0 wcwidth-0.8.2 websockets-16.1.1
2026-08-02T04:56:38.735014Z 01O [32;1m$ bash .gitlab/scripts/shell-harnesses.sh[0;m
2026-08-02T04:56:38.813383Z 01O === build_xmpp_allow_from tests ===
2026-08-02T04:56:38.815488Z 01O   PASS: owner-only (csv)
2026-08-02T04:56:38.827568Z 01O   PASS: owner+one (csv)
2026-08-02T04:56:38.846923Z 01O   PASS: owner+two (csv)
2026-08-02T04:56:38.857320Z 01O   PASS: dedupe-owner
2026-08-02T04:56:38.875852Z 01O   PASS: dedupe-extra
2026-08-02T04:56:38.893386Z 01O   PASS: trim-whitespace
2026-08-02T04:56:38.893397Z 01O === build_xmpp_allow_from_json tests ===
2026-08-02T04:56:38.901953Z 01O   PASS: json owner-only
2026-08-02T04:56:38.924718Z 01O   PASS: json owner+two
2026-08-02T04:56:38.947599Z 01O   PASS: json dedupe
2026-08-02T04:56:38.947612Z 01O === write_tenant_lunarwing_env fixture tests ===
2026-08-02T04:56:39.154624Z 01O   PASS: default LLM_MODEL
2026-08-02T04:56:39.161115Z 01O   PASS: default GATEWAY_HOST
2026-08-02T04:56:39.166038Z 01O   PASS: default XMPP_ALLOW_FROM
2026-08-02T04:56:39.171805Z 01O   PASS: default coordination disabled
2026-08-02T04:56:39.177010Z 01O   PASS: default coordination agent ID
2026-08-02T04:56:39.183377Z 01O   PASS: coordination admin token generated
2026-08-02T04:56:39.188008Z 01O   PASS: coordination admin port allocated
2026-08-02T04:56:39.333545Z 01O   PASS: flag LLM_MODEL
2026-08-02T04:56:39.339384Z 01O   PASS: flag GATEWAY_HOST
2026-08-02T04:56:39.344667Z 01O   PASS: flag XMPP_ALLOW_FROM
2026-08-02T04:56:39.832754Z 01O   PASS: rerun preserves LLM_MODEL
2026-08-02T04:56:39.838896Z 01O   PASS: rerun preserves GATEWAY_HOST
2026-08-02T04:56:39.844319Z 01O   PASS: rerun preserves XMPP_ALLOW_FROM
2026-08-02T04:56:39.850127Z 01O   PASS: rerun preserves COORD_ENABLED
2026-08-02T04:56:39.855423Z 01O   PASS: rerun preserves COORD_AGENT_ID
2026-08-02T04:56:39.861006Z 01O   PASS: rerun preserves COORD_CHANNELS
2026-08-02T04:56:39.866076Z 01O   PASS: rerun preserves COORD_ADMIN_TOKEN
2026-08-02T04:56:39.871492Z 01O   PASS: rerun preserves LW2STUDYMCP_HOME
2026-08-02T04:56:39.876806Z 01O   PASS: rerun preserves LW2STUDYMCP_STATE_HOME
2026-08-02T04:56:39.882150Z 01O   PASS: rerun preserves LW2STUDYMCP_ALLOW_WRITE
2026-08-02T04:56:40.139693Z 01O   PASS: rerun preserves DARKIRC_ADAPTER_SECRET
2026-08-02T04:56:40.139720Z 01O === coordination env backfill tests ===
2026-08-02T04:56:40.194252Z 01O   PASS: backfill coordination disabled
2026-08-02T04:56:40.199704Z 01O   PASS: backfill coordination agent ID
2026-08-02T04:56:40.204331Z 01O   PASS: backfill coordination channels
2026-08-02T04:56:40.210161Z 01O   PASS: backfill coordination aliases
2026-08-02T04:56:40.215195Z 01O   PASS: backfill coordination admin users
2026-08-02T04:56:40.220989Z 01O   PASS: backfill coordination token
2026-08-02T04:56:40.228999Z 01O   PASS: backfill coordination admin port
2026-08-02T04:56:40.279193Z 01O   PASS: backfill token remains stable
2026-08-02T04:56:40.283272Z 01O   PASS: backfill env mode
2026-08-02T04:56:40.333265Z 01O   PASS: backfill repairs empty token
2026-08-02T04:56:40.339758Z 01O   PASS: backfill repairs empty admin port
2026-08-02T04:56:40.342886Z 01O   PASS: backfill keeps one token key
2026-08-02T04:56:40.346969Z 01O   PASS: backfill keeps one port key
2026-08-02T04:56:40.346980Z 01O === XMPP_JID preservation tests ===
2026-08-02T04:56:40.524150Z 01O   PASS: first write honours explicit JID
2026-08-02T04:56:40.722992Z 01O   PASS: rerun preserves XMPP_JID
2026-08-02T04:56:40.902623Z 01O   PASS: empty-arg rerun preserves XMPP_JID
2026-08-02T04:56:40.945498Z 01O   PASS: bridge writer preserves XMPP_JID
2026-08-02T04:56:41.181820Z 01O   PASS: explicit JID overrides stored
2026-08-02T04:56:41.356043Z 01O   PASS: first write defaults to xmpp.localhost
2026-08-02T04:56:41.366045Z 01O   PASS: resolver: no file -> default
2026-08-02T04:56:41.380379Z 01O   PASS: resolver: stored wins over default
2026-08-02T04:56:41.382590Z 01O   PASS: resolver: explicit wins over stored
2026-08-02T04:56:41.382596Z 01O === write_tenant_bridge_env fixture tests ===
2026-08-02T04:56:41.450973Z 01O   PASS: bridge json owner-only
2026-08-02T04:56:41.515791Z 01O   PASS: bridge json owner+two
2026-08-02T04:56:41.515828Z 01O === per-tenant worker gating (registry) tests ===
2026-08-02T04:56:41.550559Z 01O   PASS: old tenant nanocode OFF
2026-08-02T04:56:41.622116Z 01O   PASS: old tenant pebble OFF
2026-08-02T04:56:41.692418Z 01O   PASS: old tenant opencode OFF
2026-08-02T04:56:41.764682Z 01O   PASS: retired nanocode remains OFF
2026-08-02T04:56:41.836928Z 01O   PASS: selected pebble ON
2026-08-02T04:56:42.121552Z 01O   PASS: resume enable opencode
2026-08-02T04:56:42.341195Z 01O   PASS: resume enable idempotent
2026-08-02T04:56:42.390221Z 01O   PASS: resume no leak nanocode
2026-08-02T04:56:42.437710Z 01O   PASS: registry remains valid JSON after enable
2026-08-02T04:56:42.437723Z 01O === coordination admin port migration tests ===
2026-08-02T04:56:42.567826Z 01O   PASS: v13 registry version
2026-08-02T04:56:42.635788Z 01O   PASS: v13 coord_admin rename
2026-08-02T04:56:42.703897Z 01O   PASS: v13 removes reserved_9
2026-08-02T04:56:42.823728Z 01O   PASS: ports_get resolves coord_admin
2026-08-02T04:56:42.893286Z 01O   PASS: v13 repairs missing extended_ports
2026-08-02T04:56:43.246430Z 01O   PASS: ports_get derives coord_admin from extended_base
2026-08-02T04:56:43.577393Z 01O   PASS: missing coord_admin port rejected
2026-08-02T04:56:43.582814Z 01O   PASS: failed backfill hardens env mode
2026-08-02T04:56:43.586708Z 01O   PASS: failed backfill leaves token absent
2026-08-02T04:56:43.589691Z 01O   PASS: ports_enable_worker rejects unknown worker
2026-08-02T04:56:43.589803Z 01O === CLI dispatch parsing smoke check ===
2026-08-02T04:56:43.665197Z 01O   PASS: all supported worker flags parsed
2026-08-02T04:56:43.677797Z 01O   PASS: retired Nanocode flag rejected explicitly
2026-08-02T04:56:43.751427Z 01O   PASS: add-tenant accepts --no-weechat-bootstrap
2026-08-02T04:56:43.808452Z 01O   PASS: add-tenants accepts --no-weechat-bootstrap
2026-08-02T04:56:43.808463Z 01O 
2026-08-02T04:56:43.808532Z 01O ALL TESTS PASSED
2026-08-02T04:56:44.507707Z 01O build-wasm-extensions strict fixture tests passed
2026-08-02T04:56:45.113095Z 01O check-version-bumps tests passed
2026-08-02T04:56:45.164484Z 01O === upload_tenant_darkirc_secret() regression tests ===
2026-08-02T04:56:45.164621Z 01O 
2026-08-02T04:56:45.164623Z 01O --- Test 1: guard (DarkIRC disabled) ---
2026-08-02T04:56:45.173593Z 01O   PASS: disabled returns 0
2026-08-02T04:56:45.181923Z 01O   PASS: disabled performs no POST
2026-08-02T04:56:45.181937Z 01O 
2026-08-02T04:56:45.181939Z 01O --- Test 2: guard (empty secret) ---
2026-08-02T04:56:45.201708Z 01O   PASS: empty secret returns 0
2026-08-02T04:56:45.206637Z 01O   PASS: empty secret performs no POST
2026-08-02T04:56:45.206646Z 01O 
2026-08-02T04:56:45.206648Z 01O --- Test 3: guard (missing token) ---
2026-08-02T04:56:45.235139Z 01O   PASS: missing token returns 1
2026-08-02T04:56:45.242014Z 01O   PASS: missing token performs no POST
2026-08-02T04:56:45.247061Z 01O   PASS: missing token warns
2026-08-02T04:56:45.247072Z 01O 
2026-08-02T04:56:45.247074Z 01O --- Test 4: happy path (payload + headers + URL) ---
2026-08-02T04:56:45.422723Z 01O   PASS: happy path returns 0
2026-08-02T04:56:45.430029Z 01O   PASS: happy path performs exactly one POST
2026-08-02T04:56:45.506153Z 01O   PASS: body has secrets.darkirc_adapter_secret (string, non-empty)
2026-08-02T04:56:45.577738Z 01O   PASS: body.fields is empty object
2026-08-02T04:56:45.621212Z 01O   PASS: transmitted secret matches fixture (value redacted in output)
2026-08-02T04:56:45.630726Z 01O   PASS: uses POST method
2026-08-02T04:56:45.630736Z 01O   PASS: sets Content-Type: application/json
2026-08-02T04:56:45.630738Z 01O   PASS: argv contains no fixture token
2026-08-02T04:56:45.630740Z 01O   PASS: argv contains no fixture secret
2026-08-02T04:56:45.641088Z 01O   PASS: Authorization header carries fixture token (value redacted)
2026-08-02T04:56:45.641099Z 01O   PASS: auth FD sets Bearer scheme
2026-08-02T04:56:45.649855Z 01O   PASS: posts to /api/extensions/darkirc/setup
2026-08-02T04:56:45.655354Z 01O   PASS: requests the gateway port (not http)
2026-08-02T04:56:45.655363Z 01O 
2026-08-02T04:56:45.655364Z 01O --- Test 5: failure handling (no credential leak) ---
2026-08-02T04:56:45.804806Z 01O   PASS: failure returns 1
2026-08-02T04:56:45.807098Z 01O   PASS: failure emits a warning
2026-08-02T04:56:45.807102Z 01O   PASS: function did not echo the fixture secret on failure
2026-08-02T04:56:45.807104Z 01O 
2026-08-02T04:56:45.807105Z 01O --- Test 6: idempotency (re-run) ---
2026-08-02T04:56:46.056786Z 01O   PASS: first run returns 0
2026-08-02T04:56:46.056802Z 01O   PASS: second run returns 0
2026-08-02T04:56:46.062632Z 01O   PASS: two runs => two idempotent POSTs
2026-08-02T04:56:46.062646Z 01O 
2026-08-02T04:56:46.062647Z 01O === RESULT: 0 failure(s) ===
2026-08-02T04:56:46.628077Z 01O ALL TESTS PASSED
2026-08-02T04:56:46.640814Z 01O === import-tenant dry-run flag passthrough ===
2026-08-02T04:56:46.944456Z 01O   PASS: passes --docker-group to add-tenant
2026-08-02T04:56:46.944465Z 01O   PASS: drops retired nanocode model override
2026-08-02T04:56:46.944471Z 01O   PASS: drops retired nanocode base URL override
2026-08-02T04:56:46.944472Z 01O   PASS: builds vision sidecar when requested
2026-08-02T04:56:46.944474Z 01O   PASS: injects vision manifest
2026-08-02T04:56:46.944476Z 01O   PASS: runs WeeChat preflight before start
2026-08-02T04:56:46.944476Z 01O === import-tenant worker selection reaches BOTH add-tenant and build-tenant ===
2026-08-02T04:56:47.139476Z 01O   PASS: add-tenant omits retired --with-nanocode
2026-08-02T04:56:47.139490Z 01O   PASS: add-tenant carries --with-opencode
2026-08-02T04:56:47.139491Z 01O   PASS: build-tenant omits retired --with-nanocode
2026-08-02T04:56:47.139493Z 01O   PASS: build-tenant carries --with-opencode
2026-08-02T04:56:47.139493Z 01O   PASS: add-tenant omits unselected --with-pebble
2026-08-02T04:56:47.139494Z 01O   PASS: build-tenant omits unselected --with-pebble
2026-08-02T04:56:47.142557Z 01O   PASS: import rejects retired --with-nanocode explicitly
2026-08-02T04:56:47.142559Z 01O === import-tenant explicit owner-scope dry-run ===
2026-08-02T04:56:47.371945Z 01O   PASS: plans explicit owner-scope rekey
2026-08-02T04:56:47.371956Z 01O === import-tenant auto owner-scope migration ===
2026-08-02T04:56:47.649562Z 01O   PASS: auto-migrates default owner scope
2026-08-02T04:56:47.652647Z 01O   PASS: import keeps target sidecar token
2026-08-02T04:56:47.654039Z 01O   PASS: import keeps matching daemon token
2026-08-02T04:56:47.655477Z 01O   PASS: import ignores carried sidecar token
2026-08-02T04:56:47.657347Z 01O   PASS: import injects Kimi key after provisioning
2026-08-02T04:56:47.658763Z 01O   PASS: import keeps Kimi key out of mt-admin argv
2026-08-02T04:56:47.658890Z 01O   PASS: import keeps Kimi key out of logs
2026-08-02T04:56:47.658891Z 01O === export-tenant vision manifest coverage ===
2026-08-02T04:56:47.660741Z 01O   PASS: export writes a vision manifest
2026-08-02T04:56:47.661972Z 01O   PASS: export carries VL_URL
2026-08-02T04:56:47.663161Z 01O   PASS: export carries VL_MODEL
2026-08-02T04:56:47.664661Z 01O   PASS: export does not carry host-local vision auth token
2026-08-02T04:56:47.664662Z 01O === encrypted bundle passphrase safety ===
2026-08-02T04:56:48.476830Z 01O   PASS: encrypted import reaches dry-run plan
2026-08-02T04:56:48.476846Z 01O   PASS: encrypted import output does not expose passphrase
2026-08-02T04:56:48.747465Z 01O   PASS: encrypted import rejects a wrong passphrase
2026-08-02T04:56:48.747475Z 01O === bundle layout and manifest validation ===
2026-08-02T04:56:48.857148Z 01O   PASS: import rejects unsupported manifest keys
2026-08-02T04:56:48.924486Z 01O   PASS: import rejects symbolic links in bundle layout
2026-08-02T04:56:49.041561Z 01O   PASS: import rejects links and special files in nested state
2026-08-02T04:56:49.057341Z 01O   PASS: import enforces archive resource limits before extraction
2026-08-02T04:56:49.228002Z 01O   PASS: import rejects invalid master key before provisioning
2026-08-02T04:56:49.232763Z 01O   PASS: export keeps passphrase out of 7z argv
2026-08-02T04:56:49.236283Z 01O   PASS: import keeps passphrase out of 7z argv
2026-08-02T04:56:49.236292Z 01O === export rejects missing passphrase before quiescence ===
2026-08-02T04:56:49.342534Z 01O   PASS: missing passphrase fails before export mutation
2026-08-02T04:56:49.342550Z 01O   PASS: missing passphrase does not stop tenant services
2026-08-02T04:56:49.342553Z 01O === export validates source secrets before quiescence ===
2026-08-02T04:56:49.406772Z 01O   PASS: export rejects mismatched XMPP credentials
2026-08-02T04:56:49.406784Z 01O   PASS: XMPP mismatch does not stop tenant services
2026-08-02T04:56:49.413686Z 01O === production export -> encrypted import dry-run ===
2026-08-02T04:56:49.977061Z 01O   PASS: production export writes a mode-0600 .7z bundle
2026-08-02T04:56:50.597953Z 01O   PASS: production encrypted export can be imported
2026-08-02T04:56:50.598007Z 01O   PASS: production export omits retired Nanocode model
2026-08-02T04:56:50.598404Z 01O   PASS: production export preserves OpenCode model
2026-08-02T04:56:50.598667Z 01O   PASS: encrypted Kimi round-trip does not log plaintext key
2026-08-02T04:56:50.795266Z 01O   PASS: encrypted manifest preserves Kimi API key
2026-08-02T04:56:50.796684Z 01O   PASS: encrypted manifest omits retired Nanocode model
2026-08-02T04:56:50.798052Z 01O   PASS: encrypted manifest omits retired Nanocode base URL
2026-08-02T04:56:50.798119Z 01O   PASS: production export drops host-local endpoint https://localhost:7443/openai/v1
2026-08-02T04:56:50.798205Z 01O   PASS: production export drops host-local endpoint https://127.0.0.2:7443/openai/v1
2026-08-02T04:56:50.798313Z 01O   PASS: production export drops host-local endpoint https://[::1]:7443/openai/v1
2026-08-02T04:56:50.798314Z 01O 
2026-08-02T04:56:50.798358Z 01O ALL TESTS PASSED
2026-08-02T04:56:50.845242Z 01O PASS: authenticated gateway probe succeeds
2026-08-02T04:56:50.847307Z 01O PASS: gateway port and status route used
2026-08-02T04:56:50.852350Z 01O PASS: token absent from argv
2026-08-02T04:56:50.853809Z 01O PASS: token sent through curl config
2026-08-02T04:56:50.870492Z 01O PASS: transient failure is retried
2026-08-02T04:56:50.870503Z 01O PASS: one failure causes two probes
2026-08-02T04:56:50.879907Z 01O PASS: missing gateway token fails closed
2026-08-02T04:56:50.879918Z 01O PASS: missing token performs no request
2026-08-02T04:56:50.879919Z 01O RESULT: all gateway wait tests passed
2026-08-02T04:56:50.918077Z 01O PASS: OpenRC default interval
2026-08-02T04:56:50.928496Z 01O PASS: OpenRC fcrontab uses */5
2026-08-02T04:56:50.928538Z 01O PASS: systemd default interval remains unchanged
2026-08-02T04:56:50.954082Z 01O PASS: explicit interval overrides OpenRC default
2026-08-02T04:56:50.954092Z 01O RESULT: all health interval tests passed
2026-08-02T04:56:51.021737Z 01O kimi LLM overrides written to /tmp/tmp.RQ35luDn9i/env/lunarwing.env for tenant 'root'
2026-08-02T04:56:51.025428Z 01O PASS: configure-kimi updates selected model
2026-08-02T04:56:51.026893Z 01O PASS: configure-kimi preserves omitted base URL
2026-08-02T04:56:51.033111Z 01O PASS: configure-kimi preserves omitted API key
2026-08-02T04:56:51.034716Z 01O kimi worker not selected for root (skipping external-worker config)
2026-08-02T04:56:51.034718Z 01O PASS: unselected Kimi emits no daemon config
2026-08-02T04:56:51.116027Z 01O PASS: systemd Kimi keeps rootless UID mapping
2026-08-02T04:56:51.117772Z 01O PASS: systemd Kimi loads protected runtime env
2026-08-02T04:56:51.120945Z 01O PASS: runtime env preserves agent token literally
2026-08-02T04:56:51.122618Z 01O PASS: runtime env preserves Kimi API key literally
2026-08-02T04:56:51.127426Z 01O PASS: Kimi runtime env is private
2026-08-02T04:56:51.128954Z 01O PASS: systemd unit does not inline Kimi API key
2026-08-02T04:56:51.131023Z 01O PASS: rootless Kimi workspace is private and setgid
2026-08-02T04:56:51.135608Z 01O PASS: rootful Kimi workspace uses image UID and GID
2026-08-02T04:56:51.141457Z 01O PASS: rootful OpenCode workspace uses image UID and GID
2026-08-02T04:56:51.151001Z 01O PASS: legacy quoted onboarding value is decoded
2026-08-02T04:56:51.160851Z 01O PASS: systemd-safe quoted value is decoded
2026-08-02T04:56:51.162777Z 01O PASS: Kimi builder forwards profile/version against requested source root
2026-08-02T04:56:51.168736Z 01O PASS: build-tenant forwards Kimi toolchain flag
2026-08-02T04:56:51.169024Z 01O PASS: build-all forwards Kimi toolchain flag
2026-08-02T04:56:51.169356Z 01O PASS: build-kimi-worker forwards toolchain and version flags
2026-08-02T04:56:51.172569Z 01O PASS: add-tenant reads Kimi API key from file
2026-08-02T04:56:51.174419Z 01O PASS: Kimi builder accepts source-root contract
2026-08-02T04:56:51.176213Z 01O PASS: tenant build passes tenant checkout to Kimi builder
2026-08-02T04:56:51.177930Z 01O PASS: OpenCode builder accepts source-root contract
2026-08-02T04:56:51.179393Z 01O PASS: tenant build passes tenant checkout to OpenCode builder
2026-08-02T04:56:51.181033Z 01O PASS: build-all rejects mismatched dirty worker trees
2026-08-02T04:56:51.183143Z 01O PASS: Kimi image pins build-arg default
2026-08-02T04:56:51.184722Z 01O PASS: Kimi image installs selected package version
2026-08-02T04:56:51.186231Z 01O PASS: Kimi image records toolchain profile
2026-08-02T04:56:51.187835Z 01O PASS: Kimi toolchain packages match OpenCode zlib development support
2026-08-02T04:56:51.198198Z 01O PASS: help documents toolchains for aggregate and worker builds
2026-08-02T04:56:51.198788Z 01O PASS: help documents file-based Kimi API key input only
2026-08-02T04:56:51.200735Z 01O PASS: Kimi API key stays out of container runtime argv
2026-08-02T04:56:51.200737Z 01O RESULT: all Kimi mt-admin tests passed
2026-08-02T04:56:51.607347Z 01O PASS: build-regenerated Cargo.lock is accepted
2026-08-02T04:56:51.607364Z 01O PASS: unstaged Cargo.lock change is restored
2026-08-02T04:56:51.607366Z 01O PASS: untracked runtime directories are preserved
2026-08-02T04:56:51.607368Z 01O PASS: restored lockfile is reported
2026-08-02T04:56:51.640785Z 01O PASS: tracked source edit blocks checkout with details
2026-08-02T04:56:51.640809Z 01O PASS: gate preserves whole tree when source edit exists
2026-08-02T04:56:51.673947Z 01O PASS: staged Cargo.lock edit is treated as intentional
2026-08-02T04:56:51.676985Z 01O PASS: staged Cargo.lock edit is preserved
2026-08-02T04:56:51.784752Z 01O PASS: upgrade rejects dirty tree before stopping tenant
2026-08-02T04:56:51.875731Z 01O PASS: upgrade validates target layout before stopping tenant
2026-08-02T04:56:51.939556Z 01O PASS: upgrade rejects untracked target collision before stopping tenant
2026-08-02T04:56:52.037340Z 01O PASS: actual checkout failure restarts previous tenant version
2026-08-02T04:56:52.187807Z 01O PASS: upgrade pins fetched target commit across stop and checkout
2026-08-02T04:56:52.187824Z 01O PASS: upgrade rebuilds selected OpenCode and Kimi workers with strict WASM build
2026-08-02T04:56:52.187826Z 01O RESULT: all mt-admin upgrade clean-tree tests passed
2026-08-02T04:56:52.289787Z 01O PASS: systemctl:alice stop lunarwing-opencode-alice.service
2026-08-02T04:56:52.295504Z 01O PASS: systemctl:alice stop --no-block lunarwing-opencode-alice.service
2026-08-02T04:56:52.295516Z 01O PASS: ctr:alice stop --time 0 lunarwing-opencode-alice
2026-08-02T04:56:52.295526Z 01O PASS: systemctl:alice stop lunarwing-opencode-alice.service
2026-08-02T04:56:52.295663Z 01O PASS: systemctl:alice stop --no-block lunarwing-opencode-alice.service before ctr:alice stop --time 0 lunarwing-opencode-alice
2026-08-02T04:56:52.295830Z 01O PASS: ctr:alice stop --time 0 lunarwing-opencode-alice before systemctl:alice stop lunarwing-opencode-alice.service
2026-08-02T04:56:52.301888Z 01O PASS: rc:lunarwing-opencode-alice-sup stop
2026-08-02T04:56:52.301895Z 01O PASS: ctr:alice stop lunarwing-opencode-alice
2026-08-02T04:56:52.301921Z 01O PASS: rc:lunarwing-opencode-alice-sup stop before ctr:alice stop lunarwing-opencode-alice
2026-08-02T04:56:52.308098Z 01O PASS: rc:lunarwing-opencode-alice-sup stop
2026-08-02T04:56:52.308103Z 01O PASS: ctr:alice stop --time 0 lunarwing-opencode-alice
2026-08-02T04:56:52.308260Z 01O PASS: rc:lunarwing-opencode-alice-sup stop before ctr:alice stop --time 0 lunarwing-opencode-alice
2026-08-02T04:56:52.314189Z 01O PASS: start-opencode:alice
2026-08-02T04:56:52.319686Z 01O PASS: start-kimi:alice
2026-08-02T04:56:52.326675Z 01O PASS: rc:lunarwing-kimi-alice-sup stop
2026-08-02T04:56:52.326682Z 01O PASS: ctr:alice stop lunarwing-kimi-alice
2026-08-02T04:56:52.332227Z 01O PASS: ctr:alice stop lunarwing-opencode-alice
2026-08-02T04:56:52.339952Z 01O PASS: unknown worker rejected
2026-08-02T04:56:52.347933Z 01O PASS: retired Nanocode worker rejected
2026-08-02T04:56:52.350995Z 01O PASS: retired Nanocode Quadlet rejected
2026-08-02T04:56:52.366583Z 01O PASS: store:root Docker store:lunarwing-nanocode-alice:docker
2026-08-02T04:56:52.366672Z 01O PASS: store:root Podman store:lunarwing-nanocode-alice:podman
2026-08-02T04:56:52.366824Z 01O PASS: store:tenant alice rootless Podman store:lunarwing-nanocode-alice:sudo -u alice env HOME=/tmp/tmp.sJGlyHLS7g/home/alice XDG_RUNTIME_DIR=/run/user/1000 podman
2026-08-02T04:56:52.381884Z 01O PASS: systemctl:alice stop lunarwing-nanocode-alice.service
2026-08-02T04:56:52.391376Z 01O PASS: rc:lunarwing-nanocode-alice-sup stop
2026-08-02T04:56:52.391379Z 01O PASS: rc:lunarwing-nanocode-alice stop
2026-08-02T04:56:52.424336Z 01O PASS: no ctr:alice rm -f lunarwing-nanocode-alice
2026-08-02T04:56:52.424343Z 01O PASS: ctr:alice rm -f lunarwing-pebble-alice
2026-08-02T04:56:52.424345Z 01O PASS: ctr:alice rm -f lunarwing-opencode-alice
2026-08-02T04:56:52.424352Z 01O PASS: ctr:alice rm -f lunarwing-kimi-alice
2026-08-02T04:56:52.428978Z 01O PASS: stop-systemd:alice before retire-nanocode:alice
2026-08-02T04:56:52.429241Z 01O PASS: stop-vision:alice before retire-nanocode:alice
2026-08-02T04:56:52.429608Z 01O PASS: stop-pebble:alice before retire-nanocode:alice
2026-08-02T04:56:52.429872Z 01O PASS: stop-opencode:alice before retire-nanocode:alice
2026-08-02T04:56:52.430197Z 01O PASS: stop-kimi:alice before retire-nanocode:alice
2026-08-02T04:56:52.430460Z 01O PASS: stop-postgres:alice before retire-nanocode:alice
2026-08-02T04:56:52.433489Z 01O PASS: retirement failure reported after shutdown
2026-08-02T04:56:52.433705Z 01O PASS: stop-openrc:alice before retire-nanocode:alice
2026-08-02T04:56:52.434043Z 01O PASS: stop-postgres:alice before retire-nanocode:alice
2026-08-02T04:56:52.434046Z 01O RESULT: all worker lifecycle tests passed
2026-08-02T04:56:52.837725Z 01O PASS: Nanocode TOML block and nested tables removed
2026-08-02T04:56:52.841437Z 01O PASS: original config retained as rollback copy
2026-08-02T04:56:52.846453Z 01O PASS: config rollback copy is private
2026-08-02T04:56:53.028542Z 01O PASS: malformed TOML blocks retirement
2026-08-02T04:56:53.028561Z 01O PASS: malformed config remains unchanged
2026-08-02T04:56:53.028574Z 01O PASS: malformed config creates no rollback copy
2026-08-02T04:56:53.191822Z 01O PASS: symlinked rollback path blocks config retirement
2026-08-02T04:56:53.191848Z 01O PASS: symlink target remains unchanged
2026-08-02T04:56:53.195822Z 01O PASS: unsafe backup leaves live config unchanged
2026-08-02T04:56:53.371128Z 01O PASS: symlinked tenant env blocks retirement
2026-08-02T04:56:53.371156Z 01O PASS: tenant env symlink target remains unchanged
2026-08-02T04:56:53.698126Z 01O PASS: Nanocode environment retired
2026-08-02T04:56:53.702483Z 01O PASS: live environment drops Nanocode keys
2026-08-02T04:56:53.704608Z 01O PASS: live environment preserves supported worker keys
2026-08-02T04:56:53.704779Z 01O PASS: legacy worker env archived
2026-08-02T04:56:53.710036Z 01O PASS: live environment hardened
2026-08-02T04:56:53.712908Z 01O PASS: environment rollback copy private
2026-08-02T04:56:53.909136Z 01O PASS: registry marks Nanocode retired
2026-08-02T04:56:53.973312Z 01O PASS: registry preserves Nanocode WSS slot
2026-08-02T04:56:54.014545Z 01O PASS: registry preserves Nanocode health slot
2026-08-02T04:56:54.081492Z 01O PASS: registry preserves supported workers
2026-08-02T04:56:54.081675Z 01O PASS: retirement order stops before mutation
2026-08-02T04:56:54.081817Z 01O PASS: container stop failure blocks retirement
2026-08-02T04:56:54.081821Z 01O PASS: stop failure prevents config and registry mutation
2026-08-02T04:56:54.081858Z 01O RESULT: all Nanocode retirement tests passed
2026-08-02T04:56:54.094295Z 01O   PASS: bootstrap defaults enabled
2026-08-02T04:56:54.096368Z 01O   PASS: disabled setting forwards opt-out
2026-08-02T04:56:54.096381Z 01O ALL TESTS PASSED
2026-08-02T04:56:56.434317Z 01O Study MCP worker mount tests passed
2026-08-02T04:56:56.650604Z 01O PASS: WASM builder succeeds with direct Preview 2 artifact
2026-08-02T04:56:56.655305Z 01O PASS: WASM builder uses cargo build for wasm32-wasip2
2026-08-02T04:56:56.805001Z 01O PASS: WASM builder propagates Cargo failure
2026-08-02T04:56:56.805076Z 01O PASS: failed WASM build removes stale source artifact
2026-08-02T04:56:56.937318Z 01O PASS: WASM builder rejects missing registered source
2026-08-02T04:56:57.014947Z 01O PASS: strict tenant build fails after WASM build error
2026-08-02T04:56:57.014963Z 01O PASS: strict tenant build blocks stale WASM install
2026-08-02T04:56:57.026927Z 01O PASS: non-strict tenant build preserves installed WASM after build error
2026-08-02T04:56:57.030683Z 01O PASS: strict WASM mode rejects missing --with-wasm
2026-08-02T04:56:57.034614Z 01O PASS: upgrade-tenant requires successful WASM build
2026-08-02T04:56:57.038965Z 01O PASS: upgrade-tenant rebuilds selected OpenCode worker
2026-08-02T04:56:57.045233Z 01O PASS: build-tenant CLI forwards strict WASM mode
2026-08-02T04:56:57.047426Z 01O PASS: upgrade-tenant.sh requires a fresh WASM build
2026-08-02T04:56:57.052375Z 01O PASS: upgrade-tenant-version.sh requires a fresh WASM build
2026-08-02T04:56:57.054734Z 01O PASS: upgrade-tenant-kageho.sh requires a fresh WASM build
2026-08-02T04:56:57.058794Z 01O PASS: upgrade-tenant-starforce.sh requires a fresh WASM build
2026-08-02T04:56:57.062708Z 01O PASS: upgrade-tenant-sunburst.sh requires a fresh WASM build
2026-08-02T04:56:57.069585Z 01O PASS: live upgrade cannot bypass fresh WASM build
2026-08-02T04:56:57.069590Z 01O RESULT: all WASM upgrade build tests passed
2026-08-02T04:56:57.146568Z 01O === systemd rendering and dependency gating ===
2026-08-02T04:56:57.216944Z 01O   PASS: enabled systemd backend rendered
2026-08-02T04:56:57.216952Z 01O   PASS: enabled systemd adapter rendered
2026-08-02T04:56:57.220301Z 01O   PASS: enabled systemd main wants backend
2026-08-02T04:56:57.221902Z 01O   PASS: enabled systemd main wants adapter
2026-08-02T04:56:57.338596Z 01O   PASS: disabled systemd backend removed
2026-08-02T04:56:57.338609Z 01O   PASS: disabled systemd adapter removed
2026-08-02T04:56:57.338611Z 01O   PASS: disabled systemd backend enable link removed
2026-08-02T04:56:57.338613Z 01O   PASS: disabled systemd adapter enable link removed
2026-08-02T04:56:57.340226Z 01O   PASS: disabled systemd main drops backend dependency
2026-08-02T04:56:57.343551Z 01O   PASS: disabled systemd main drops adapter dependency
2026-08-02T04:56:57.343557Z 01O === OpenRC rendering and dependency gating ===
2026-08-02T04:56:57.434576Z 01O   PASS: enabled OpenRC backend rendered
2026-08-02T04:56:57.434587Z 01O   PASS: enabled OpenRC adapter rendered
2026-08-02T04:56:57.436525Z 01O   PASS: enabled OpenRC main orders after backend
2026-08-02T04:56:57.438190Z 01O   PASS: enabled OpenRC main needs backend
2026-08-02T04:56:57.440057Z 01O   PASS: OpenRC main consumes configured optional dependencies
2026-08-02T04:56:57.527834Z 01O   PASS: disabled OpenRC backend removed
2026-08-02T04:56:57.527845Z 01O   PASS: disabled OpenRC adapter removed
2026-08-02T04:56:57.527846Z 01O   PASS: disabled OpenRC backend runlevel link removed
2026-08-02T04:56:57.527847Z 01O   PASS: disabled OpenRC adapter runlevel link removed
2026-08-02T04:56:57.529318Z 01O   PASS: disabled OpenRC main drops backend ordering
2026-08-02T04:56:57.532206Z 01O   PASS: disabled OpenRC main drops backend need
2026-08-02T04:56:57.532209Z 01O === strict systemd lifecycle ===
2026-08-02T04:56:57.597144Z 01O   PASS: systemd disable succeeds when stop/disable succeed
2026-08-02T04:56:57.598878Z 01O   PASS: systemd disable calls stop
2026-08-02T04:56:57.600293Z 01O   PASS: systemd disable removes enablement
2026-08-02T04:56:57.605041Z 01O   PASS: systemd disable propagates stop failure
2026-08-02T04:56:57.606113Z 01O   PASS: systemd disable fails when user manager is unavailable
2026-08-02T04:56:57.608422Z 01O   PASS: systemd disable fails when unit query fails
2026-08-02T04:56:57.609748Z 01O   PASS: systemd enable succeeds when all operations succeed
2026-08-02T04:56:57.611398Z 01O   PASS: systemd enable calls enable
2026-08-02T04:56:57.612752Z 01O   PASS: systemd enable starts backend
2026-08-02T04:56:57.614281Z 01O   PASS: systemd enable starts adapter
2026-08-02T04:56:57.615574Z 01O   PASS: systemd enable propagates enable failure
2026-08-02T04:56:57.622254Z 01O   PASS: systemd disable is idempotent when units are absent
2026-08-02T04:56:57.623316Z 01O   PASS: systemd enable fails when rendered units are absent
2026-08-02T04:56:57.623343Z 01O === strict OpenRC lifecycle ===
2026-08-02T04:56:57.740594Z 01O   PASS: OpenRC disable succeeds when stop/runlevel removal succeed
2026-08-02T04:56:57.742309Z 01O   PASS: OpenRC disable stops adapter
2026-08-02T04:56:57.745522Z 01O   PASS: OpenRC disable removes runlevel
2026-08-02T04:56:57.747345Z 01O   PASS: OpenRC disable removes non-default runlevel
2026-08-02T04:56:57.747702Z 01O   PASS: OpenRC disable propagates stop failure
2026-08-02T04:56:57.748117Z 01O   PASS: OpenRC enable succeeds when all operations succeed
2026-08-02T04:56:57.751783Z 01O   PASS: OpenRC enable adds backend runlevel
2026-08-02T04:56:57.755306Z 01O   PASS: OpenRC enable starts adapter
2026-08-02T04:56:57.755699Z 01O   PASS: OpenRC enable propagates start failure
2026-08-02T04:56:57.779103Z 01O   PASS: OpenRC disable is idempotent when scripts are absent
2026-08-02T04:56:57.779123Z 01O   PASS: OpenRC enable fails when rendered scripts are absent
2026-08-02T04:56:57.779125Z 01O === renderer exit containment ===
2026-08-02T04:56:57.780233Z 01O   PASS: reconcile converts renderer exit into catchable failure
2026-08-02T04:56:57.780341Z 01O === transactional command ordering ===
2026-08-02T04:56:57.789048Z 01O   PASS: disable transaction stops before changing registry
2026-08-02T04:56:57.789063Z 01O   PASS: failed stop leaves registry enabled
2026-08-02T04:56:57.789090Z 01O   PASS: failed stop performs no later action
2026-08-02T04:56:57.798100Z 01O   PASS: disable transaction reports reconciliation failure
2026-08-02T04:56:57.798110Z 01O   PASS: failed disabled reconciliation restores enabled flag
2026-08-02T04:56:57.799823Z 01O   PASS: failed disabled reconciliation attempts enabled restore
2026-08-02T04:56:57.808746Z 01O   PASS: disable transaction succeeds in strict order
2026-08-02T04:56:57.808757Z 01O   PASS: successful disable leaves registry disabled
2026-08-02T04:56:57.808852Z 01O   PASS: successful disable action order
2026-08-02T04:56:57.818016Z 01O   PASS: enable transaction renders before start
2026-08-02T04:56:57.818066Z 01O   PASS: successful enable leaves registry enabled
2026-08-02T04:56:57.818196Z 01O   PASS: successful enable action order
2026-08-02T04:56:57.818198Z 01O === per-tenant lifecycle serialization ===
2026-08-02T04:56:58.038500Z 01O   PASS: same-tenant lifecycle operations do not overlap
2026-08-02T04:56:58.038517Z 01O 
2026-08-02T04:56:58.038518Z 01O ALL TESTS PASSED
2026-08-02T04:56:58.055176Z 01O === 1. valid setup ===
2026-08-02T04:56:58.387719Z 01O   PASS: valid → exit 0 (exit 0)
2026-08-02T04:56:58.387735Z 01O   PASS: relay.conf OK label
2026-08-02T04:56:58.387820Z 01O   PASS: weechat.env OK label
2026-08-02T04:56:58.388015Z 01O   PASS: literal expression shown
2026-08-02T04:56:58.388144Z 01O   PASS: api port shown
2026-08-02T04:56:58.388287Z 01O   PASS: no secret in output
2026-08-02T04:56:58.388424Z 01O   PASS: adapter health is INFO
2026-08-02T04:56:58.390974Z 01O === 2. absent relay config ===
2026-08-02T04:56:58.690400Z 01O   PASS: absent config → exit 1 (exit 1)
2026-08-02T04:56:58.690413Z 01O   PASS: relay.conf FAIL
2026-08-02T04:56:58.690422Z 01O   PASS: recovery command
2026-08-02T04:56:58.690423Z 01O   PASS: no secret in output
2026-08-02T04:56:58.692872Z 01O === 3. wrong API port ===
2026-08-02T04:56:59.031279Z 01O   PASS: wrong port → exit 1 (exit 1)
2026-08-02T04:56:59.031296Z 01O   PASS: relay.conf FAIL
2026-08-02T04:56:59.031488Z 01O   PASS: expected port mentioned
2026-08-02T04:56:59.031674Z 01O   PASS: recovery command
2026-08-02T04:56:59.031902Z 01O   PASS: no secret in output
2026-08-02T04:56:59.034490Z 01O === 4. non-loopback bind ===
2026-08-02T04:56:59.334929Z 01O   PASS: non-loopback → exit 1 (exit 1)
2026-08-02T04:56:59.334939Z 01O   PASS: relay.conf FAIL
2026-08-02T04:56:59.334940Z 01O   PASS: loopback mentioned
2026-08-02T04:56:59.335050Z 01O   PASS: no secret in output
2026-08-02T04:56:59.336472Z 01O === 5. literal-password leak ===
2026-08-02T04:56:59.619757Z 01O   PASS: password leak → exit 1 (exit 1)
2026-08-02T04:56:59.619863Z 01O   PASS: relay.conf FAIL
2026-08-02T04:56:59.619925Z 01O   PASS: leak/security risk detail
2026-08-02T04:56:59.620199Z 01O   PASS: no secret in output
2026-08-02T04:56:59.622824Z 01O === 6. missing minimal env ===
2026-08-02T04:56:59.940349Z 01O   PASS: missing env → exit 1 (exit 1)
2026-08-02T04:56:59.940360Z 01O   PASS: weechat.env FAIL
2026-08-02T04:56:59.940366Z 01O   PASS: recovery command
2026-08-02T04:56:59.940555Z 01O   PASS: no secret in output
2026-08-02T04:56:59.944492Z 01O === 7. adapter-down informational ===
2026-08-02T04:57:00.314869Z 01O   PASS: adapter-down → exit 0 (exit 0)
2026-08-02T04:57:00.314886Z 01O   PASS: adapter health is INFO not FAIL
2026-08-02T04:57:00.314896Z 01O   PASS: no adapter FAIL
2026-08-02T04:57:00.315104Z 01O   PASS: no secret in output
2026-08-02T04:57:00.319372Z 01O === 8. setup-error exit 2 ===
2026-08-02T04:57:00.325836Z 01O   PASS: missing registry → exit 2 (exit 2)
2026-08-02T04:57:00.325849Z 01O   PASS: error message
2026-08-02T04:57:00.325875Z 01O   PASS: no secret in output
2026-08-02T04:57:00.325883Z 01O === 9. empty-lw-relay-pw (no false plaintext-leak) ===
2026-08-02T04:57:00.619664Z 01O   PASS: empty-lw-pw → exit 1 (exit 1)
2026-08-02T04:57:00.619682Z 01O   PASS: no false plaintext leak
2026-08-02T04:57:00.619753Z 01O   PASS: relay.conf still OK
2026-08-02T04:57:00.619932Z 01O   PASS: weechat.env FAIL (empty pw)
2026-08-02T04:57:00.620078Z 01O   PASS: no secret in output
2026-08-02T04:57:00.620086Z 01O === 10. mismatch-pw (non-empty mismatch must FAIL) ===
2026-08-02T04:57:00.907028Z 01O   PASS: mismatch-pw → exit 1 (exit 1)
2026-08-02T04:57:00.907039Z 01O   PASS: weechat.env FAIL
2026-08-02T04:57:00.907044Z 01O   PASS: mismatch detail
2026-08-02T04:57:00.907058Z 01O   PASS: no WARN for weechat.env
2026-08-02T04:57:00.907238Z 01O   PASS: no fixture secret in output
2026-08-02T04:57:00.907283Z 01O   PASS: no mismatch secret in output
2026-08-02T04:57:00.907327Z 01O === 11. ipv6-on (IPv4 loopback with ipv6 = on) ===
2026-08-02T04:57:01.106227Z 01O   PASS: ipv6-on → exit 1 (exit 1)
2026-08-02T04:57:01.106243Z 01O   PASS: relay.conf FAIL
2026-08-02T04:57:01.106244Z 01O   PASS: ipv6 detail
2026-08-02T04:57:01.106250Z 01O   PASS: ipv6 recovery
2026-08-02T04:57:01.106361Z 01O   PASS: no secret in output
2026-08-02T04:57:01.108058Z 01O === 12. missing-ipv6-option ===
2026-08-02T04:57:01.291827Z 01O   PASS: missing-ipv6 → exit 1 (exit 1)
2026-08-02T04:57:01.291838Z 01O   PASS: relay.conf FAIL
2026-08-02T04:57:01.291839Z 01O   PASS: ipv6 detail
2026-08-02T04:57:01.291845Z 01O   PASS: ipv6 recovery
2026-08-02T04:57:01.291846Z 01O   PASS: no secret in output
2026-08-02T04:57:01.293739Z 01O 
2026-08-02T04:57:01.293742Z 01O ----------------------------------------------------------------
2026-08-02T04:57:01.293743Z 01O Tests run: 56  Failures: 0
2026-08-02T04:57:01.293743Z 01O ALL TESTS PASSED
2026-08-02T04:57:01.326996Z 01O === tenant_weechat_home tests ===
2026-08-02T04:57:01.328739Z 01O   PASS: returns .config/weechat under tenant home
2026-08-02T04:57:01.328758Z 01O === _read_env_value tests ===
2026-08-02T04:57:01.336392Z 01O   PASS: reads last value for duplicate keys
2026-08-02T04:57:01.341816Z 01O   PASS: reads first key
2026-08-02T04:57:01.345586Z 01O   PASS: missing key returns empty
2026-08-02T04:57:01.346303Z 01O   PASS: missing file returns empty
2026-08-02T04:57:01.346305Z 01O === _weechat_config_dir_has_entries tests ===
2026-08-02T04:57:01.350654Z 01O   PASS: empty dir has no entries
2026-08-02T04:57:01.357640Z 01O   PASS: dir with dotfile has entries
2026-08-02T04:57:01.363927Z 01O   PASS: dir with normal file has entries
2026-08-02T04:57:01.363935Z 01O === _weechat_validate_relay_config tests ===
2026-08-02T04:57:01.381137Z 01O   PASS: valid config accepted
2026-08-02T04:57:01.398374Z 01O   PASS: wrong port rejected
2026-08-02T04:57:01.404011Z 01O   PASS: plaintext password leak rejected
2026-08-02T04:57:01.413873Z 01O   PASS: non-loopback bind rejected
2026-08-02T04:57:01.417747Z 01O   PASS: missing relay.conf rejected
2026-08-02T04:57:01.439527Z 01O   PASS: missing [api] rejected
2026-08-02T04:57:01.448740Z 01O   PASS: missing env expression rejected
2026-08-02T04:57:01.458579Z 01O   PASS: missing allow_empty_password=off rejected
2026-08-02T04:57:01.470205Z 01O   PASS: ipv6 = on with IPv4 bind rejected
2026-08-02T04:57:01.483283Z 01O   PASS: missing ipv6 option rejected
2026-08-02T04:57:01.483295Z 01O === _weechat_generate_relay_config tests ===
2026-08-02T04:57:01.521351Z 01O   PASS: call log exists
2026-08-02T04:57:01.530549Z 01O   PASS: call args contain escaped ${env:RELAY_PASSWORD}, not plaintext
2026-08-02T04:57:01.539495Z 01O   PASS: uses /relay add api, not addreplace
2026-08-02T04:57:01.541634Z 01O   PASS: bind_address 127.0.0.1 in call args
2026-08-02T04:57:01.545438Z 01O   PASS: allow_empty_password off in call args
2026-08-02T04:57:01.547403Z 01O   PASS: relay.network.ipv6 off in call args
2026-08-02T04:57:01.552328Z 01O   PASS: ipv6 off precedes bind and relay creation
2026-08-02T04:57:01.557550Z 01O   PASS: /save and /quit present
2026-08-02T04:57:01.578287Z 01O   PASS: generated config is valid
2026-08-02T04:57:01.590979Z 01O   PASS: missing WeeChat returns instead of exiting the caller
2026-08-02T04:57:01.590989Z 01O   PASS: missing WeeChat return code
2026-08-02T04:57:01.590990Z 01O === configure_weechat_relay tests ===
2026-08-02T04:57:01.671087Z 01O   PASS: target dir created
2026-08-02T04:57:01.671097Z 01O   PASS: relay.conf exists
2026-08-02T04:57:01.685919Z 01O   PASS: promoted config is valid
2026-08-02T04:57:01.686882Z 01O   PASS: explicit recovery writes weechat.env
2026-08-02T04:57:01.686943Z 01O   PASS: explicit recovery writes only RELAY_PASSWORD
2026-08-02T04:57:01.688914Z 01O   PASS: no plaintext password in promoted dir
2026-08-02T04:57:01.692977Z 01O   PASS: temp dirs cleaned
2026-08-02T04:57:01.736112Z 01O   PASS: non-empty target fails
2026-08-02T04:57:01.743298Z 01O   PASS: conflict target unchanged
2026-08-02T04:57:01.744230Z 01O   PASS: conflict preserves existing weechat.env
2026-08-02T04:57:01.744234Z 01O   PASS: WeeChat not invoked on conflict
2026-08-02T04:57:01.785180Z 01O   PASS: legacy config without weechat.env fails
2026-08-02T04:57:01.790252Z 01O   PASS: legacy config preserved unchanged
2026-08-02T04:57:01.790353Z 01O   PASS: legacy path creates weechat.env
2026-08-02T04:57:01.790585Z 01O   PASS: legacy path weechat.env has RELAY_PASSWORD
2026-08-02T04:57:01.812824Z 01O   PASS: missing password fails
2026-08-02T04:57:01.815011Z 01O   PASS: target not created on missing password
2026-08-02T04:57:01.830005Z 01O   PASS: configuration errors return instead of exiting the caller
2026-08-02T04:57:01.830023Z 01O   PASS: configuration error return code
2026-08-02T04:57:01.915195Z 01O   PASS: generation failure causes configure failure
2026-08-02T04:57:01.915864Z 01O   PASS: target not created on generation failure
2026-08-02T04:57:01.923927Z 01O   PASS: no temp dirs on gen failure
2026-08-02T04:57:02.023000Z 01O   PASS: invalid generated config fails
2026-08-02T04:57:02.027269Z 01O   PASS: target not created on validation failure
2026-08-02T04:57:02.162967Z 01O   PASS: empty existing target succeeds
2026-08-02T04:57:02.194011Z 01O   PASS: promoted config valid for empty target
2026-08-02T04:57:02.334067Z 01O   PASS: stale-env configure succeeds (empty target)
2026-08-02T04:57:02.360401Z 01O   PASS: stale-env generated relay.conf validates
2026-08-02T04:57:02.362678Z 01O   PASS: stale-env no plaintext password in config dir
2026-08-02T04:57:02.362866Z 01O   PASS: stale-env weechat.env synchronizes to lunarwing.env
2026-08-02T04:57:02.493782Z 01O   PASS: bootstrap uses a unique temp directory
2026-08-02T04:57:02.648461Z 01O   PASS: promotion race fails safely
2026-08-02T04:57:02.648497Z 01O   PASS: promotion race preserves competing target
2026-08-02T04:57:02.648755Z 01O === CLI dispatch smoke check ===
2026-08-02T04:57:02.733848Z 01O   PASS: --help lists configure-weechat-relay
2026-08-02T04:57:02.771289Z 01O   PASS: missing argument produces usage error
2026-08-02T04:57:02.848807Z 01O   PASS: extra argument produces error
2026-08-02T04:57:04.033946Z 01O   PASS: unknown tenant handled (reached dispatch, not unknown command)
2026-08-02T04:57:04.033962Z 01O === add_tenant non-fatal auto-bootstrap ===
2026-08-02T04:57:04.095467Z 01O   PASS: weechat bootstrap called after env creation
2026-08-02T04:57:04.095481Z 01O   PASS: weechat bootstrap called before postgres
2026-08-02T04:57:04.140008Z 01O   PASS: base provisioning continues after bootstrap failure
2026-08-02T04:57:04.140020Z 01O === opt-out (--no-weechat-bootstrap) behavior ===
2026-08-02T04:57:04.191382Z 01O   PASS: opt-out does not invoke WeeChat bootstrap
2026-08-02T04:57:04.194454Z 01O   PASS: opt-out still writes minimal WeeChat env
2026-08-02T04:57:04.198225Z 01O   PASS: opt-out continues base provisioning
2026-08-02T04:57:04.198566Z 01O   PASS: opt-out summary is explicit
2026-08-02T04:57:04.198576Z 01O === opt-in live WeeChat smoke ===
2026-08-02T04:57:04.198779Z 01O   SKIP: set RUN_LIVE_WEECHAT=1 to enable
2026-08-02T04:57:04.198780Z 01O 
2026-08-02T04:57:04.198833Z 01O ALL TESTS PASSED
2026-08-02T04:57:04.248911Z 01O === _write_weechat_env tests ===
2026-08-02T04:57:04.268430Z 01O   PASS: weechat.env exists
2026-08-02T04:57:04.272276Z 01O   PASS: exactly one line
2026-08-02T04:57:04.275788Z 01O   PASS: contains RELAY_PASSWORD assignment
2026-08-02T04:57:04.284498Z 01O   PASS: weechat.env contains only RELAY_PASSWORD
2026-08-02T04:57:04.284517Z 01O   PASS: mode is 0600
2026-08-02T04:57:04.295367Z 01O   PASS: weechat.env writer uses a unique temp file
2026-08-02T04:57:04.312223Z 01O   PASS: weechat.env directory conflict fails safely
2026-08-02T04:57:04.318260Z 01O   PASS: weechat.env conflict leaves directory empty
2026-08-02T04:57:04.318269Z 01O === systemd renderer helper ===
2026-08-02T04:57:04.351536Z 01O   PASS: systemd unit file exists
2026-08-02T04:57:04.358233Z 01O   PASS: unit name is lunarwing-weechat-<tenant>
2026-08-02T04:57:04.359753Z 01O   PASS: systemd unit has EnvironmentFile=.../weechat.env
2026-08-02T04:57:04.361802Z 01O   PASS: systemd does not point at lunarwing.env
2026-08-02T04:57:04.371718Z 01O   PASS: systemd starts WeeChat through session-restore helper
2026-08-02T04:57:04.379184Z 01O   PASS: systemd contains weechat_home path
2026-08-02T04:57:04.384180Z 01O   PASS: systemd ExecStop uses graceful stop helper
2026-08-02T04:57:04.389722Z 01O   PASS: systemd ExecStop passes weechat-home and tmux-socket
2026-08-02T04:57:04.393515Z 01O   PASS: systemd sets TimeoutStopSec
2026-08-02T04:57:04.393527Z 01O === OpenRC renderer helper ===
2026-08-02T04:57:04.431654Z 01O   PASS: OpenRC script exists
2026-08-02T04:57:04.436373Z 01O   PASS: script name is lunarwing-weechat-<tenant>
2026-08-02T04:57:04.439386Z 01O   PASS: OpenRC script passes shell syntax check
2026-08-02T04:57:04.445597Z 01O   PASS: OpenRC sets weechat_env_file=.../weechat.env
2026-08-02T04:57:04.449618Z 01O   PASS: OpenRC weechat_env_file does not point at lunarwing.env
2026-08-02T04:57:04.454321Z 01O   PASS: OpenRC sets the post-drop env launcher
2026-08-02T04:57:04.457700Z 01O   PASS: OpenRC starts WeeChat through the post-drop env launcher
2026-08-02T04:57:04.467803Z 01O   PASS: OpenRC does not read tenant-controlled WeeChat env content as root
2026-08-02T04:57:04.470452Z 01O   PASS: OpenRC retains tenant privilege-drop configuration (command_user)
2026-08-02T04:57:04.484220Z 01O   PASS: OpenRC starts WeeChat through session-restore helper
2026-08-02T04:57:04.490930Z 01O   PASS: OpenRC stop() uses graceful stop helper
2026-08-02T04:57:04.504484Z 01O   PASS: OpenRC stop() drops to tenant UID via --chuid
2026-08-02T04:57:04.508381Z 01O   PASS: OpenRC stop() does not use filter-only --user
2026-08-02T04:57:04.516142Z 01O   PASS: OpenRC stop() propagates helper failure
2026-08-02T04:57:04.519860Z 01O   PASS: OpenRC stop() retains tmux kill-session fallback
2026-08-02T04:57:04.519990Z 01O === OpenRC tenant stop propagation ===
2026-08-02T04:57:04.520855Z 01O   PASS: stop_tenant_openrc propagates WeeChat stop failure
2026-08-02T04:57:04.520951Z 01O 
2026-08-02T04:57:04.521012Z 01O ALL TESTS PASSED
2026-08-02T04:57:04.532297Z 01O SKIP: set RUN_LIVE_WEECHAT_RESTORE=1 to test real session restoration
2026-08-02T04:57:04.553913Z 01O === normal start without saved session ===
2026-08-02T04:57:04.569939Z 01O   PASS: starts normally when no upgrade state exists
2026-08-02T04:57:04.569951Z 01O === restore start with saved session ===
2026-08-02T04:57:04.586887Z 01O   PASS: adds --upgrade when saved session exists
2026-08-02T04:57:04.586901Z 01O === invalid arguments ===
2026-08-02T04:57:04.593669Z 01O   PASS: requires --weechat-bin
2026-08-02T04:57:04.600950Z 01O   PASS: rejects non-executable WeeChat binary
2026-08-02T04:57:04.607418Z 01O   PASS: rejects unknown argument
2026-08-02T04:57:04.607426Z 01O 
2026-08-02T04:57:04.607427Z 01O ALL TESTS PASSED
2026-08-02T04:57:04.634622Z 01O === no session: exit 0 ===
2026-08-02T04:57:04.657030Z 01O   PASS: returns 0 when no session exists
2026-08-02T04:57:04.657045Z 01O === absent tmux server: exit 0 ===
2026-08-02T04:57:04.675535Z 01O   PASS: treats missing tmux server as already stopped
2026-08-02T04:57:04.675550Z 01O === tmux probe error: nonzero ===
2026-08-02T04:57:04.696329Z 01O   PASS: reports tmux probe error
2026-08-02T04:57:04.696343Z 01O === required and validated arguments ===
2026-08-02T04:57:04.702781Z 01O   PASS: rejects missing --weechat-home
2026-08-02T04:57:04.709738Z 01O   PASS: rejects missing --tmux-socket
2026-08-02T04:57:04.716697Z 01O   PASS: rejects non-numeric timeout
2026-08-02T04:57:04.723453Z 01O   PASS: rejects unknown argument
2026-08-02T04:57:04.723458Z 01O === live FIFO: exact upgrade command preserves session ===
2026-08-02T04:57:04.776844Z 01O   PASS: writes exact WeeChat FIFO upgrade command
2026-08-02T04:57:04.776855Z 01O   PASS: graceful exit avoids kill-session fallback
2026-08-02T04:57:04.776857Z 01O === stale live PID: selects FIFO associated with tmux pane ===
2026-08-02T04:57:04.829342Z 01O   PASS: ignores unrelated live-PID FIFO
2026-08-02T04:57:04.829351Z 01O === blocked FIFO: bounded fallback ===
2026-08-02T04:57:05.902805Z 01O   PASS: blocked FIFO falls back without hanging
2026-08-02T04:57:05.902829Z 01O === failed fallback: nonzero ===
2026-08-02T04:57:05.945770Z 01O   PASS: reports surviving tmux session
2026-08-02T04:57:05.945787Z 01O 
2026-08-02T04:57:05.945789Z 01O ALL TESTS PASSED
2026-08-02T04:57:06.030706Z 01O == H2: --enable-darkirc flag flip on existing tenant ==
2026-08-02T04:57:06.349411Z 01O   ok   - fresh tenant base_port (alpha)
2026-08-02T04:57:06.391707Z 01O   ok   - alpha enable_darkirc initially false
2026-08-02T04:57:06.629597Z 01O   ok   - resume reuses base_port (alpha)
2026-08-02T04:57:06.666406Z 01O   ok   - alpha enable_darkirc flips to true
2026-08-02T04:57:06.843808Z 01O   ok   - resume w/o flag keeps base_port (alpha)
2026-08-02T04:57:06.873600Z 01O   ok   - alpha stays enabled (no silent downgrade)
2026-08-02T04:57:07.044408Z 01O   ok   - second tenant base_port (beta)
2026-08-02T04:57:07.074191Z 01O   ok   - beta enable_darkirc true on create
2026-08-02T04:57:07.170118Z 01O   ok   - idempotent: registry not rewritten
2026-08-02T04:57:07.212855Z 01O   ok   - beta still enabled after idempotent call
2026-08-02T04:57:07.503549Z 01E allocated port block 10020-10029 for tenant 'gamma'
2026-08-02T04:57:07.698254Z 01E allocated port block 10030-10039 for tenant 'delta'
2026-08-02T04:57:07.701569Z 01O   ok   - parallel allocations receive distinct port blocks
2026-08-02T04:57:07.902332Z 01O migrating port registry -> v6.1 (assign darkirc_adapter from extended slot 0) ...
2026-08-02T04:57:07.961216Z 01O port registry migrated to v6.1 (darkirc_adapter added)
2026-08-02T04:57:07.986523Z 01O migrating port registry -> v12 (add kimi_wss + kimi_health from dedicated range) ...
2026-08-02T04:57:08.067324Z 01O port registry migrated to v12 (kimi_wss + kimi_health dedicated in kimi_range 30000-39999)
2026-08-02T04:57:08.144063Z 01O port registry migrated to v13 (coord_admin dedicated at extended_base+9)
2026-08-02T04:57:08.144099Z 01O   ok   - v11 repair acquires registry lock
2026-08-02T04:57:08.211254Z 01O   ok   - v11 repair assigns darkirc adapter
2026-08-02T04:57:08.211268Z 01O 
2026-08-02T04:57:08.211269Z 01O RESULT: PASS (13 assertions)
2026-08-02T04:57:08.225044Z 01O ── Python unit tests ──
2026-08-02T04:57:08.866985Z 01E test_current_llm_and_worker_overrides_round_trip (lunarwing_mt_onboard.tests.TestConfigSerialization.test_current_llm_and_worker_overrides_round_trip) ... ok
2026-08-02T04:57:08.866999Z 01E test_from_dict_accepts_supported_string_workers (lunarwing_mt_onboard.tests.TestConfigSerialization.test_from_dict_accepts_supported_string_workers) ... ok
2026-08-02T04:57:08.867001Z 01E test_from_dict_rejects_retired_nanocode_worker (lunarwing_mt_onboard.tests.TestConfigSerialization.test_from_dict_rejects_retired_nanocode_worker) ... ok
2026-08-02T04:57:08.867031Z 01E test_json_file_permissions (lunarwing_mt_onboard.tests.TestConfigSerialization.test_json_file_permissions) ... ok
2026-08-02T04:57:08.868008Z 01E test_json_is_valid_format (lunarwing_mt_onboard.tests.TestConfigSerialization.test_json_is_valid_format) ... ok
2026-08-02T04:57:08.868446Z 01E test_old_json_without_weechat_field_defaults_false (lunarwing_mt_onboard.tests.TestConfigSerialization.test_old_json_without_weechat_field_defaults_false) ... ok
2026-08-02T04:57:08.868456Z 01E test_roundtrip_json (lunarwing_mt_onboard.tests.TestConfigSerialization.test_roundtrip_json) ... ok
2026-08-02T04:57:08.868963Z 01E test_to_dict_serializes_workers_as_strings (lunarwing_mt_onboard.tests.TestConfigSerialization.test_to_dict_serializes_workers_as_strings) ... ok
2026-08-02T04:57:08.868990Z 01E test_weechat_bootstrap_defaults_false (lunarwing_mt_onboard.tests.TestConfigSerialization.test_weechat_bootstrap_defaults_false) ... ok
2026-08-02T04:57:08.869095Z 01E test_weechat_bootstrap_opt_out_round_trips (lunarwing_mt_onboard.tests.TestConfigSerialization.test_weechat_bootstrap_opt_out_round_trips) ... ok
2026-08-02T04:57:08.869175Z 01E test_build_darkirc_uses_tenant_context (lunarwing_mt_onboard.tests.TestProvisionerArgs.test_build_darkirc_uses_tenant_context) ... ok
2026-08-02T04:57:08.869512Z 01E test_current_llm_and_worker_overrides_are_forwarded (lunarwing_mt_onboard.tests.TestProvisionerArgs.test_current_llm_and_worker_overrides_are_forwarded) ... ok
2026-08-02T04:57:08.869791Z 01E test_custom_gateway_host_is_forwarded_to_mt_admin (lunarwing_mt_onboard.tests.TestProvisionerArgs.test_custom_gateway_host_is_forwarded_to_mt_admin) ... ok
2026-08-02T04:57:08.870051Z 01E test_custom_xmpp_jid_is_forwarded_to_mt_admin (lunarwing_mt_onboard.tests.TestProvisionerArgs.test_custom_xmpp_jid_is_forwarded_to_mt_admin) ... ok
2026-08-02T04:57:08.870298Z 01E test_darkirc_flag_is_forwarded_to_mt_admin (lunarwing_mt_onboard.tests.TestProvisionerArgs.test_darkirc_flag_is_forwarded_to_mt_admin) ... ok
2026-08-02T04:57:08.870556Z 01E test_kimi_api_key_uses_secret_injection_not_argv (lunarwing_mt_onboard.tests.TestProvisionerArgs.test_kimi_api_key_uses_secret_injection_not_argv) ... ok
2026-08-02T04:57:08.873131Z 01E test_lunarwing_model_is_forwarded_to_mt_admin (lunarwing_mt_onboard.tests.TestProvisionerArgs.test_lunarwing_model_is_forwarded_to_mt_admin) ... ok
2026-08-02T04:57:08.873424Z 01E test_no_workers_selected_emits_no_worker_flags (lunarwing_mt_onboard.tests.TestProvisionerArgs.test_no_workers_selected_emits_no_worker_flags) ... ok
2026-08-02T04:57:08.873664Z 01E test_skip_build_also_skips_start (lunarwing_mt_onboard.tests.TestProvisionerArgs.test_skip_build_also_skips_start) ... ok
2026-08-02T04:57:08.874272Z 01E test_unrelated_subprocess_does_not_inherit_kawarimi_secrets (lunarwing_mt_onboard.tests.TestProvisionerArgs.test_unrelated_subprocess_does_not_inherit_kawarimi_secrets) ... ok
2026-08-02T04:57:08.878205Z 01E test_weechat_bootstrap_opt_out_is_forwarded (lunarwing_mt_onboard.tests.TestProvisionerArgs.test_weechat_bootstrap_opt_out_is_forwarded) ... ok
2026-08-02T04:57:08.878468Z 01E test_worker_selection_is_forwarded_to_add_tenant (lunarwing_mt_onboard.tests.TestProvisionerArgs.test_worker_selection_is_forwarded_to_add_tenant) ... ok
2026-08-02T04:57:08.878736Z 01E test_write_env_values_rejects_multiline_secret (lunarwing_mt_onboard.tests.TestProvisionerArgs.test_write_env_values_rejects_multiline_secret) ... ok
2026-08-02T04:57:08.879110Z 01E test_write_env_values_uses_systemd_safe_quotes (lunarwing_mt_onboard.tests.TestProvisionerArgs.test_write_env_values_uses_systemd_safe_quotes) ... ok
2026-08-02T04:57:08.879910Z 01E test_xmpp_password_is_written_to_daemon_and_bridge_envs (lunarwing_mt_onboard.tests.TestProvisionerArgs.test_xmpp_password_is_written_to_daemon_and_bridge_envs) ... ok
2026-08-02T04:57:08.880509Z 01E test_generate_master_key_is_hex (lunarwing_mt_onboard.tests.TestSecretsHelpers.test_generate_master_key_is_hex) ... ok
2026-08-02T04:57:08.880548Z 01E test_generate_master_key_length (lunarwing_mt_onboard.tests.TestSecretsHelpers.test_generate_master_key_length) ... ok
2026-08-02T04:57:08.880629Z 01E test_generate_master_key_randomness (lunarwing_mt_onboard.tests.TestSecretsHelpers.test_generate_master_key_randomness) ... ok
2026-08-02T04:57:08.880631Z 01E test_is_valid_master_key_rejects_non_hex (lunarwing_mt_onboard.tests.TestSecretsHelpers.test_is_valid_master_key_rejects_non_hex) ... ok
2026-08-02T04:57:08.880640Z 01E test_is_valid_master_key_rejects_short (lunarwing_mt_onboard.tests.TestSecretsHelpers.test_is_valid_master_key_rejects_short) ... ok
2026-08-02T04:57:08.880675Z 01E test_mask_secret_empty (lunarwing_mt_onboard.tests.TestSecretsHelpers.test_mask_secret_empty) ... ok
2026-08-02T04:57:08.880737Z 01E test_mask_secret_long (lunarwing_mt_onboard.tests.TestSecretsHelpers.test_mask_secret_long) ... ok
2026-08-02T04:57:08.880769Z 01E test_mask_secret_short (lunarwing_mt_onboard.tests.TestSecretsHelpers.test_mask_secret_short) ... ok
2026-08-02T04:57:08.880861Z 01E test_empty_name_rejected (lunarwing_mt_onboard.tests.TestTenantNameValidation.test_empty_name_rejected) ... ok
2026-08-02T04:57:08.880863Z 01E test_reserved_prefix_rejected (lunarwing_mt_onboard.tests.TestTenantNameValidation.test_reserved_prefix_rejected) ... ok
2026-08-02T04:57:08.881028Z 01E test_underscore_rejected (lunarwing_mt_onboard.tests.TestTenantNameValidation.test_underscore_rejected) ... ok
2026-08-02T04:57:08.881052Z 01E test_uppercase_rejected (lunarwing_mt_onboard.tests.TestTenantNameValidation.test_uppercase_rejected) ... ok
2026-08-02T04:57:08.881092Z 01E test_valid_hyphenated_name (lunarwing_mt_onboard.tests.TestTenantNameValidation.test_valid_hyphenated_name) ... ok
2026-08-02T04:57:08.881142Z 01E test_valid_simple_name (lunarwing_mt_onboard.tests.TestTenantNameValidation.test_valid_simple_name) ... ok
2026-08-02T04:57:08.881319Z 01E test_missing_script_raises (lunarwing_mt_onboard.import_tenant_tests.TestEnsureImportScript.test_missing_script_raises) ... ok
2026-08-02T04:57:08.881378Z 01E test_override_is_used_when_executable (lunarwing_mt_onboard.import_tenant_tests.TestEnsureImportScript.test_override_is_used_when_executable) ... ok
2026-08-02T04:57:08.882037Z 01E test_all_optional_flags_are_forwarded (lunarwing_mt_onboard.import_tenant_tests.TestImportArgs.test_all_optional_flags_are_forwarded) ... ok
2026-08-02T04:57:08.882281Z 01E test_apply_stage_omits_dry_run_and_start (lunarwing_mt_onboard.import_tenant_tests.TestImportArgs.test_apply_stage_omits_dry_run_and_start) ... ok
2026-08-02T04:57:08.882586Z 01E test_auto_yes_can_be_disabled_for_non_web_callers (lunarwing_mt_onboard.import_tenant_tests.TestImportArgs.test_auto_yes_can_be_disabled_for_non_web_callers) ... ok
2026-08-02T04:57:08.882826Z 01E test_default_is_bundle_first_dry_run_and_yes (lunarwing_mt_onboard.import_tenant_tests.TestImportArgs.test_default_is_bundle_first_dry_run_and_yes) ... ok
2026-08-02T04:57:08.883076Z 01E test_start_and_old_stopped_are_independent_flags (lunarwing_mt_onboard.import_tenant_tests.TestImportArgs.test_start_and_old_stopped_are_independent_flags) ... ok
2026-08-02T04:57:08.883631Z 01E test_defaults_are_dry_run_stage_only_and_noninteractive (lunarwing_mt_onboard.import_tenant_tests.TestImportConfigDefaults.test_defaults_are_dry_run_stage_only_and_noninteractive) ... ok
2026-08-02T04:57:08.883655Z 01E test_passphrase_is_not_serialized_or_represented (lunarwing_mt_onboard.import_tenant_tests.TestImportConfigSerialization.test_passphrase_is_not_serialized_or_represented) ... ok
2026-08-02T04:57:08.883668Z 01E test_roundtrip_json_preserves_all_fields (lunarwing_mt_onboard.import_tenant_tests.TestImportConfigSerialization.test_roundtrip_json_preserves_all_fields) ... ok
2026-08-02T04:57:08.884093Z 01E test_to_dict_roundtrip_uses_safe_defaults (lunarwing_mt_onboard.import_tenant_tests.TestImportConfigSerialization.test_to_dict_roundtrip_uses_safe_defaults) ... ok
2026-08-02T04:57:08.884128Z 01E test_empty_bundle_rejected (lunarwing_mt_onboard.import_tenant_tests.TestImportConfigValidation.test_empty_bundle_rejected) ... ok
2026-08-02T04:57:08.884223Z 01E test_invalid_optional_name_rejected (lunarwing_mt_onboard.import_tenant_tests.TestImportConfigValidation.test_invalid_optional_name_rejected) ... ok
2026-08-02T04:57:08.884225Z 01E test_multiline_passphrase_rejected (lunarwing_mt_onboard.import_tenant_tests.TestImportConfigValidation.test_multiline_passphrase_rejected) ... ok
2026-08-02T04:57:08.884229Z 01E test_retired_nanocode_is_rejected (lunarwing_mt_onboard.import_tenant_tests.TestImportConfigValidation.test_retired_nanocode_is_rejected) ... ok
2026-08-02T04:57:08.884310Z 01E test_valid_bundle_without_name (lunarwing_mt_onboard.import_tenant_tests.TestImportConfigValidation.test_valid_bundle_without_name) ... ok
2026-08-02T04:57:08.884320Z 01E test_run_import_supplies_passphrase_through_fd (lunarwing_mt_onboard.import_tenant_tests.TestImportPhaseNames.test_run_import_supplies_passphrase_through_fd) ... ok
2026-08-02T04:57:08.887198Z 01E test_run_import_uses_import_phase_name (lunarwing_mt_onboard.import_tenant_tests.TestImportPhaseNames.test_run_import_uses_import_phase_name) ... ok
2026-08-02T04:57:08.889797Z 01E test_missing_script_raises (lunarwing_mt_onboard.upgrade_tests.TestEnsurePreflightScript.test_missing_script_raises) ... ok
2026-08-02T04:57:08.889923Z 01E test_env_override_used (lunarwing_mt_onboard.upgrade_tests.TestEnsureUpgradeScript.test_env_override_used) ... ok
2026-08-02T04:57:08.890237Z 01E test_missing_script_raises (lunarwing_mt_onboard.upgrade_tests.TestEnsureUpgradeScript.test_missing_script_raises) ... ok
2026-08-02T04:57:08.890396Z 01E test_current_config_round_trips_with_owner_only_permissions (lunarwing_mt_onboard.upgrade_tests.TestMtAdminUpgradeArgs.test_current_config_round_trips_with_owner_only_permissions) ... ok
2026-08-02T04:57:08.890801Z 01E test_target_accepts_branch_tag_or_commit_but_not_options (lunarwing_mt_onboard.upgrade_tests.TestMtAdminUpgradeArgs.test_target_accepts_branch_tag_or_commit_but_not_options) ... ok
2026-08-02T04:57:08.890938Z 01E test_v2_ref_and_current_flags_are_forwarded (lunarwing_mt_onboard.upgrade_tests.TestMtAdminUpgradeArgs.test_v2_ref_and_current_flags_are_forwarded) ... ok
2026-08-02T04:57:08.891236Z 01E test_preflight_argv_shape (lunarwing_mt_onboard.upgrade_tests.TestPreflightArgs.test_preflight_argv_shape) ... ok
2026-08-02T04:57:08.891547Z 01E test_bare_number_rejected (lunarwing_mt_onboard.upgrade_tests.TestTargetTagValidation.test_bare_number_rejected) ... ok
2026-08-02T04:57:08.891552Z 01E test_branch_rejected (lunarwing_mt_onboard.upgrade_tests.TestTargetTagValidation.test_branch_rejected) ... ok
2026-08-02T04:57:08.891587Z 01E test_empty_tag_accepted (lunarwing_mt_onboard.upgrade_tests.TestTargetTagValidation.test_empty_tag_accepted) ... ok
2026-08-02T04:57:08.891675Z 01E test_exact_release_accepted (lunarwing_mt_onboard.upgrade_tests.TestTargetTagValidation.test_exact_release_accepted) ... ok
2026-08-02T04:57:08.891677Z 01E test_method_on_config (lunarwing_mt_onboard.upgrade_tests.TestTargetTagValidation.test_method_on_config) ... ok
2026-08-02T04:57:08.891745Z 01E test_prerelease_rejected (lunarwing_mt_onboard.upgrade_tests.TestTargetTagValidation.test_prerelease_rejected) ... ok
2026-08-02T04:57:08.891767Z 01E test_apply_adds_flag (lunarwing_mt_onboard.upgrade_tests.TestUpgradeArgs.test_apply_adds_flag) ... ok
2026-08-02T04:57:08.892061Z 01E test_dry_run_omits_apply (lunarwing_mt_onboard.upgrade_tests.TestUpgradeArgs.test_dry_run_omits_apply) ... ok
2026-08-02T04:57:08.892318Z 01E test_target_source_force_yes_forwarded (lunarwing_mt_onboard.upgrade_tests.TestUpgradeArgs.test_target_source_force_yes_forwarded) ... ok
2026-08-02T04:57:08.892612Z 01E test_default_is_dry_run (lunarwing_mt_onboard.upgrade_tests.TestUpgradeConfigDefaults.test_default_is_dry_run) ... ok
2026-08-02T04:57:08.892614Z 01E test_default_runs_preflight (lunarwing_mt_onboard.upgrade_tests.TestUpgradeConfigDefaults.test_default_runs_preflight) ... ok
2026-08-02T04:57:08.892669Z 01E test_from_dict_defaults_missing_fields (lunarwing_mt_onboard.upgrade_tests.TestUpgradeConfigSerialization.test_from_dict_defaults_missing_fields) ... ok
2026-08-02T04:57:08.892719Z 01E test_json_file_permissions (lunarwing_mt_onboard.upgrade_tests.TestUpgradeConfigSerialization.test_json_file_permissions) ... ok
2026-08-02T04:57:08.893407Z 01E test_roundtrip_json (lunarwing_mt_onboard.upgrade_tests.TestUpgradeConfigSerialization.test_roundtrip_json) ... ok
2026-08-02T04:57:08.893741Z 01E test_to_dict_roundtrip (lunarwing_mt_onboard.upgrade_tests.TestUpgradeConfigSerialization.test_to_dict_roundtrip) ... ok
2026-08-02T04:57:08.893823Z 01E test_empty_tenant_rejected (lunarwing_mt_onboard.upgrade_tests.TestUpgradeConfigValidation.test_empty_tenant_rejected) ... ok
2026-08-02T04:57:08.893826Z 01E test_invalid_source_override (lunarwing_mt_onboard.upgrade_tests.TestUpgradeConfigValidation.test_invalid_source_override) ... ok
2026-08-02T04:57:08.893828Z 01E test_invalid_target (lunarwing_mt_onboard.upgrade_tests.TestUpgradeConfigValidation.test_invalid_target) ... ok
2026-08-02T04:57:08.893836Z 01E test_invalid_tenant (lunarwing_mt_onboard.upgrade_tests.TestUpgradeConfigValidation.test_invalid_tenant) ... ok
2026-08-02T04:57:08.893849Z 01E test_valid_config (lunarwing_mt_onboard.upgrade_tests.TestUpgradeConfigValidation.test_valid_config) ... ok
2026-08-02T04:57:08.893969Z 01E test_run_preflight_uses_preflight_phase_name (lunarwing_mt_onboard.upgrade_tests.TestUpgradePhaseNames.test_run_preflight_uses_preflight_phase_name) ... ok
2026-08-02T04:57:08.896783Z 01E test_run_upgrade_uses_distinct_phase_names (lunarwing_mt_onboard.upgrade_tests.TestUpgradePhaseNames.test_run_upgrade_uses_distinct_phase_names) ... ok
2026-08-02T04:57:08.901425Z 01E test_run_preflight_returns_phase_result (lunarwing_mt_onboard.upgrade_runtime_tests.TestRunPreflight.test_run_preflight_returns_phase_result) ... ok
2026-08-02T04:57:08.904179Z 01E test_force_continues_past_failed_preflight (lunarwing_mt_onboard.upgrade_runtime_tests.TestRunUpgrade.test_force_continues_past_failed_preflight) ... ok
2026-08-02T04:57:08.909094Z 01E test_preflight_failure_stops_unless_forced (lunarwing_mt_onboard.upgrade_runtime_tests.TestRunUpgrade.test_preflight_failure_stops_unless_forced) ... ok
2026-08-02T04:57:08.911590Z 01E test_upgrade_runs_preflight_then_upgrade (lunarwing_mt_onboard.upgrade_runtime_tests.TestRunUpgrade.test_upgrade_runs_preflight_then_upgrade) ... ok
2026-08-02T04:57:08.916227Z 01E test_non_interactive_upgrade_requires_apply (lunarwing_mt_onboard.upgrade_runtime_tests.TestUpgradeFlow.test_non_interactive_upgrade_requires_apply) ... ok
2026-08-02T04:57:08.921353Z 01O Upgrade requires explicit --apply confirmation.
2026-08-02T04:57:08.924815Z 01O upgrade-tenant alpha --target v2.0.2.0 --source-repo /srv/lunarwing --no-backup 
2026-08-02T04:57:08.924823Z 01O --skip-render
2026-08-02T04:57:08.926037Z 01O       Upgrade result       
2026-08-02T04:57:08.926044Z 01O ┏━━━━━━━━━┳━━━━━━━━┳━━━━━━┓
2026-08-02T04:57:08.926050Z 01O ┃ Phase   ┃ Status ┃ Code ┃
2026-08-02T04:57:08.926053Z 01O ┡━━━━━━━━━╇━━━━━━━━╇━━━━━━┩
2026-08-02T04:57:08.926117Z 01O │ upgrade │   OK   │    0 │
2026-08-02T04:57:08.926120Z 01O └─────────┴────────┴──────┘
2026-08-02T04:57:08.926152Z 01O Upgrade command completed.
2026-08-02T04:57:08.921344Z 01E test_non_interactive_v2_upgrade_uses_mt_admin_and_verifies (lunarwing_mt_onboard.upgrade_runtime_tests.TestUpgradeFlow.test_non_interactive_v2_upgrade_uses_mt_admin_and_verifies) ... ok
2026-08-02T04:57:08.927885Z 01O           Post-upgrade verification          
2026-08-02T04:57:08.927889Z 01O ┏━━━━━━━━━━━━━━━━━━━━━━━━━┳━━━━━━━━┳━━━━━━━━┓
2026-08-02T04:57:08.927896Z 01O ┃ Check                   ┃ Status ┃ Detail ┃
2026-08-02T04:57:08.927900Z 01O ┡━━━━━━━━━━━━━━━━━━━━━━━━━╇━━━━━━━━╇━━━━━━━━┩
2026-08-02T04:57:08.927905Z 01O │ service lunarwing-alpha │  PASS  │ active │
2026-08-02T04:57:08.927908Z 01O └─────────────────────────┴────────┴────────┘
2026-08-02T04:57:08.931743Z 01O       Upgrade result       
2026-08-02T04:57:08.931756Z 01O ┏━━━━━━━━━┳━━━━━━━━┳━━━━━━┓
2026-08-02T04:57:08.931761Z 01O ┃ Phase   ┃ Status ┃ Code ┃
2026-08-02T04:57:08.931764Z 01O ┡━━━━━━━━━╇━━━━━━━━╇━━━━━━┩
2026-08-02T04:57:08.931766Z 01O │ upgrade │   OK   │    0 │
2026-08-02T04:57:08.931768Z 01O └─────────┴────────┴──────┘
2026-08-02T04:57:08.931770Z 01O Upgrade command completed.
2026-08-02T04:57:08.933757Z 01O                            Post-upgrade verification                            
2026-08-02T04:57:08.933762Z 01O ┏━━━━━━━━━━━━━━━━━━━━━━━━━┳━━━━━━━━┳━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓
2026-08-02T04:57:08.933767Z 01O ┃ Check                   ┃ Status ┃ Detail                                    ┃
2026-08-02T04:57:08.933769Z 01O ┡━━━━━━━━━━━━━━━━━━━━━━━━━╇━━━━━━━━╇━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┩
2026-08-02T04:57:08.933773Z 01O │ gateway port            │  FAIL  │ gateway allocation not found in ports     │
2026-08-02T04:57:08.933775Z 01O │                         │        │ registry                                  │
2026-08-02T04:57:08.933777Z 01O │ service lunarwing-alpha │  PASS  │ active                                    │
2026-08-02T04:57:08.933779Z 01O └─────────────────────────┴────────┴───────────────────────────────────────────┘
2026-08-02T04:57:08.927871Z 01E test_upgrade_fails_verification_without_gateway_allocation (lunarwing_mt_onboard.upgrade_runtime_tests.TestUpgradeFlow.test_upgrade_fails_verification_without_gateway_allocation) ... ok
2026-08-02T04:57:08.933926Z 01E test_derive_key_deterministic (lunarwing_mt_onboard.secrets_tests.TestCrypto.test_derive_key_deterministic)
2026-08-02T04:57:08.933928Z 01E Same master_key + salt should produce the same derived key. ... ok
2026-08-02T04:57:08.941784Z 01E test_derive_key_different_salts (lunarwing_mt_onboard.secrets_tests.TestCrypto.test_derive_key_different_salts)
2026-08-02T04:57:08.941787Z 01E Different salts should produce different keys. ... ok
2026-08-02T04:57:08.941789Z 01E test_encrypt_decrypt_roundtrip (lunarwing_mt_onboard.secrets_tests.TestCrypto.test_encrypt_decrypt_roundtrip)
2026-08-02T04:57:08.941791Z 01E Verify we can decrypt what we encrypt. ... ok
2026-08-02T04:57:08.945381Z 01E test_encrypt_produces_different_ciphertexts (lunarwing_mt_onboard.secrets_tests.TestCrypto.test_encrypt_produces_different_ciphertexts)
2026-08-02T04:57:08.945384Z 01E Same plaintext should produce different ciphertexts (random salt+nonce). ... ok
2026-08-02T04:57:08.945386Z 01E test_encrypt_returns_correct_lengths (lunarwing_mt_onboard.secrets_tests.TestCrypto.test_encrypt_returns_correct_lengths)
2026-08-02T04:57:08.945388Z 01E Encrypted value = nonce (12) + ciphertext + tag (16). ... ok
2026-08-02T04:57:08.945408Z 01E test_returns_bool (lunarwing_mt_onboard.secrets_tests.TestEnsureDependencies.test_returns_bool) ... ok
2026-08-02T04:57:08.955495Z 01E test_empty_tenants (lunarwing_mt_onboard.secrets_tests.TestListTenants.test_empty_tenants) ... ok
2026-08-02T04:57:08.955929Z 01E test_malformed_json (lunarwing_mt_onboard.secrets_tests.TestListTenants.test_malformed_json) ... ok
2026-08-02T04:57:08.956229Z 01E test_no_ports_file (lunarwing_mt_onboard.secrets_tests.TestListTenants.test_no_ports_file) ... ok
2026-08-02T04:57:08.956524Z 01E test_populated_tenants (lunarwing_mt_onboard.secrets_tests.TestListTenants.test_populated_tenants) ... ok
2026-08-02T04:57:08.956887Z 01E test_tenant_gateway_host_reads_tenant_env (lunarwing_mt_onboard.secrets_tests.TestListTenants.test_tenant_gateway_host_reads_tenant_env) ... ok
2026-08-02T04:57:08.957138Z 01E test_tenant_gateway_port_reads_registry (lunarwing_mt_onboard.secrets_tests.TestListTenants.test_tenant_gateway_port_reads_registry) ... ok
2026-08-02T04:57:08.957532Z 01E test_missing_env_file (lunarwing_mt_onboard.secrets_tests.TestParseTenantEnv.test_missing_env_file) ... ok
2026-08-02T04:57:08.957633Z 01E test_parse_quotes_stripped (lunarwing_mt_onboard.secrets_tests.TestParseTenantEnv.test_parse_quotes_stripped)
2026-08-02T04:57:08.957636Z 01E Verify quote-stripping logic by testing the parser directly. ... ok
2026-08-02T04:57:08.957854Z 01E test_parse_valid_env (lunarwing_mt_onboard.secrets_tests.TestParseTenantEnv.test_parse_valid_env) ... ok
2026-08-02T04:57:08.958360Z 01E test_empty_rejected (lunarwing_mt_onboard.secrets_tests.TestSecretNameValidation.test_empty_rejected) ... ok
2026-08-02T04:57:08.958366Z 01E test_spaces_rejected (lunarwing_mt_onboard.secrets_tests.TestSecretNameValidation.test_spaces_rejected) ... ok
2026-08-02T04:57:08.958484Z 01E test_special_chars_rejected (lunarwing_mt_onboard.secrets_tests.TestSecretNameValidation.test_special_chars_rejected) ... ok
2026-08-02T04:57:08.958486Z 01E test_valid_simple_name (lunarwing_mt_onboard.secrets_tests.TestSecretNameValidation.test_valid_simple_name) ... ok
2026-08-02T04:57:08.958507Z 01E test_valid_with_hyphen (lunarwing_mt_onboard.secrets_tests.TestSecretNameValidation.test_valid_with_hyphen) ... ok
2026-08-02T04:57:08.958509Z 01E test_valid_with_slash (lunarwing_mt_onboard.secrets_tests.TestSecretNameValidation.test_valid_with_slash) ... ok
2026-08-02T04:57:08.958677Z 01E 
2026-08-02T04:57:08.958679Z 01E ----------------------------------------------------------------------
2026-08-02T04:57:08.958681Z 01E Ran 114 tests in 0.092s
2026-08-02T04:57:08.958682Z 01E 
2026-08-02T04:57:08.958683Z 01E OK
2026-08-02T04:57:08.992074Z 01O 
2026-08-02T04:57:08.992087Z 01O ── Module import check ──
2026-08-02T04:57:09.370461Z 01O All modules imported successfully
2026-08-02T04:57:09.385426Z 01O 
2026-08-02T04:57:09.385430Z 01O ── Non-interactive dry-run (--skip-build --skip-start) ──
2026-08-02T04:57:09.831604Z 01O Parsed: Namespace(non_interactive=True, accept_defaults=False, resume=None, save=None, skip_build=True, skip_start=True, command=None)
2026-08-02T04:57:09.850072Z 01O 
2026-08-02T04:57:09.850084Z 01O ── Current mt-admin upgrade parser check ──
2026-08-02T04:57:10.275920Z 01O Parsed: Namespace(non_interactive=True, accept_defaults=False, resume=None, save=None, skip_build=False, skip_start=False, command='upgrade', tenant='alpha', target='v2.0.2.0', source_repo='/srv/lunarwing', no_backup=True, skip_render=True, apply=True, yes=True)
2026-08-02T04:57:10.293339Z 01O 
2026-08-02T04:57:10.293351Z 01O ── Secrets subcommand parser check ──
2026-08-02T04:57:10.715359Z 01O Parsed: Namespace(non_interactive=True, accept_defaults=False, resume=None, save=None, skip_build=False, skip_start=False, command='secrets', tenant='alpha', secretname='gotify_app_token', secretvalue='testval', yes=False)
2026-08-02T04:57:10.736447Z 01O 
2026-08-02T04:57:10.736456Z 01O ✓ Smoke test passed
2026-08-02T04:57:10.754783Z 01O === automatic job calculation ===
2026-08-02T04:57:10.754794Z 01O PASS: 75% of 16 CPUs is 12
2026-08-02T04:57:10.754799Z 01O PASS: 7108MB permits four automatic jobs
2026-08-02T04:57:10.754800Z 01O PASS: memory caps the recommendation
2026-08-02T04:57:10.754810Z 01O PASS: single CPU never produces zero jobs
2026-08-02T04:57:10.754857Z 01O PASS: insufficient memory produces no automatic jobs
2026-08-02T04:57:10.755979Z 01O PASS: automatic build fails below the per-job memory floor
2026-08-02T04:57:10.757440Z 01O PASS: low-memory failure suggests an explicit override
2026-08-02T04:57:10.759714Z 01O PASS: explicit low-memory override is warned and honored
2026-08-02T04:57:10.759829Z 01O PASS: unknown memory uses the CPU recommendation
2026-08-02T04:57:10.759830Z 01O === inherited cgroup limits ===
2026-08-02T04:57:10.764364Z 01O PASS: ancestor CPU quota is applied
2026-08-02T04:57:10.766373Z 01O PASS: most restrictive ancestor memory headroom is applied
2026-08-02T04:57:10.766375Z 01O === platform and safety validation ===
2026-08-02T04:57:10.768057Z 01O PASS: non-Linux hosts are rejected
2026-08-02T04:57:10.769474Z 01O PASS: Linux-only error is explicit
2026-08-02T04:57:10.771698Z 01O PASS: Linux aarch64 uses the supported shared path
2026-08-02T04:57:10.773048Z 01O PASS: destructive and eval commands are absent
2026-08-02T04:57:10.773049Z 01O === mocked end-to-end invocation ===
2026-08-02T04:57:10.851358Z 01O PASS: Cargo receives a locked release binary build
2026-08-02T04:57:10.852570Z 01O PASS: Cargo receives jobs as a separate argument
2026-08-02T04:57:10.852578Z 01O PASS: missing target directory is created
2026-08-02T04:57:10.854038Z 01O PASS: command-line jobs override BUILD_JOBS
2026-08-02T04:57:10.868383Z 01O PASS: target symlinks resolving to the filesystem root are rejected
2026-08-02T04:57:10.870369Z 01O PASS: root target rejection is explicit
2026-08-02T04:57:10.921369Z 01O PASS: dry run does not invoke cargo build
2026-08-02T04:57:10.924386Z 01O PASS: dry run prints validated command
2026-08-02T04:57:11.000749Z 01O PASS: target and log paths containing spaces are preserved
2026-08-02T04:57:11.001975Z 01O PASS: spaced binary path is reported
2026-08-02T04:57:11.095865Z 01O PASS: configured Cargo target binary is discovered
2026-08-02T04:57:11.096982Z 01O PASS: configured Cargo target does not produce a false warning
2026-08-02T04:57:11.174717Z 01O PASS: verbose Cargo pipeline preserves the build failure status
2026-08-02T04:57:11.178176Z 01O PASS: failed build output is retained
2026-08-02T04:57:11.293265Z 01O PASS: concurrent clean/build use of a target is rejected
2026-08-02T04:57:11.296431Z 01O PASS: lock contention error identifies the target
2026-08-02T04:57:11.307583Z 01O PASS: malicious BUILD_JOBS is rejected without execution
2026-08-02T04:57:11.310692Z 01O PASS: invalid jobs error is actionable
2026-08-02T04:57:11.326749Z 01O PASS: missing option values return usage error status
2026-08-02T04:57:11.330008Z 01O PASS: missing jobs value is explained
2026-08-02T04:57:11.347986Z 01O PASS: invalid profiles return usage error status
2026-08-02T04:57:11.351303Z 01O PASS: invalid profile is explained
2026-08-02T04:57:11.351309Z 01O 
2026-08-02T04:57:11.351310Z 01O 37 passed, 0 failed
2026-08-02T04:57:11.366864Z 01O 
2026-08-02T04:57:11.366873Z 01O [1m━━━ regression (test-self-heal.sh) ━━━[0m
2026-08-02T04:57:29.544855Z 01O PASS: A: unhealthy systemd sub-unit remediated
2026-08-02T04:57:29.544963Z 01O PASS: A: gateway maps to lunarwing
2026-08-02T04:57:29.544965Z 01O PASS: A: healthy sub-unit not restarted
2026-08-02T04:57:29.544967Z 01O PASS: A: no-remedy not restarted
2026-08-02T04:57:29.544968Z 01O PASS: A: pass-2 jq does not abort
2026-08-02T04:57:29.544970Z 01O FAIL: B1: exponential schedules next_attempt_at in [0,60]
2026-08-02T04:57:29.544971Z 01O       got [-1] not in [0,60]
2026-08-02T04:57:29.544973Z 01O PASS: B2: backoff was 0 (jitter) — no gate expected
2026-08-02T04:57:29.544976Z 01O PASS: B3: linear strategy is deterministic (now+base)
2026-08-02T04:57:29.544977Z 01O PASS: C1: first observation defers (grace)
2026-08-02T04:57:29.544979Z 01O PASS: C1: no restart during grace
2026-08-02T04:57:29.544980Z 01O PASS: C2: restart fires once grace met
2026-08-02T04:57:29.544981Z 01O PASS: D1: flapping detected
2026-08-02T04:57:29.544983Z 01O PASS: D2: flapping escalates
2026-08-02T04:57:29.544984Z 01O PASS: D3: flapping not restarted
2026-08-02T04:57:29.544985Z 01O PASS: E1: verification detects still-unhealthy
2026-08-02T04:57:29.544987Z 01O PASS: E1: failed verify counts as a retry
2026-08-02T04:57:29.544988Z 01O PASS: E2: healthy verify clears the service
2026-08-02T04:57:29.544989Z 01O PASS: E2: state cleared after verified-healthy restart
2026-08-02T04:57:29.544991Z 01O PASS: F1: stale non-escalated pruned, escalated kept
2026-08-02T04:57:29.544992Z 01O PASS: F2: --prune-ttl 0 disables pruning
2026-08-02T04:57:29.544994Z 01O PASS: G1: systemd tenant unit restarts as tenant user
2026-08-02T04:57:29.544995Z 01O PASS: G2: uses systemd --user bus
2026-08-02T04:57:29.544996Z 01O PASS: G3: OpenRC tenant service via rc-service
2026-08-02T04:57:29.544998Z 01O PASS: H1: service the report calls healthy is cleared (report-as-truth)
2026-08-02T04:57:29.545000Z 01O PASS: H1: recovered state cleared without is-active round-trip
2026-08-02T04:57:29.545002Z 01O PASS: I1: --grace-checks 1 restarts on first observation
2026-08-02T04:57:29.545003Z 01O PASS: I2: --backoff-strategy/-base flags honored
2026-08-02T04:57:29.545005Z 01O PASS: I3: --verify-health false skips health-check (is-active only)
2026-08-02T04:57:29.545006Z 01O 
2026-08-02T04:57:29.545008Z 01O 27 passed, 1 failed
2026-08-02T04:57:29.547916Z 01O 
2026-08-02T04:57:29.547923Z 01O [1m━━━ matrix (test-self-heal-matrix.sh) ━━━[0m
2026-08-02T04:58:24.911306Z 01O === Section A: Report discovery & init ===
2026-08-02T04:58:24.911321Z 01O PASS: A1: empty report dir dies
2026-08-02T04:58:24.911323Z 01O PASS: A1: exit 1 when no report
2026-08-02T04:58:24.911323Z 01O PASS: A2: invalid JSON dies
2026-08-02T04:58:24.911324Z 01O PASS: A2: exit 1 on malformed report
2026-08-02T04:58:24.911325Z 01O PASS: A3: summary-only dir excluded from discovery
2026-08-02T04:58:24.911326Z 01O PASS: A4: explicit --report is honored over discovery
2026-08-02T04:58:24.911326Z 01O === Section B: Target selection & service mapping ===
2026-08-02T04:58:24.911327Z 01O PASS: B1: gateway → lunarwing
2026-08-02T04:58:24.911331Z 01O PASS: B2: xmpp → xmpp-bridge
2026-08-02T04:58:24.911332Z 01O PASS: B3: tensorzero → tensorzero-gateway
2026-08-02T04:58:24.911333Z 01O PASS: B4: clickhouse → clickhouse-server
2026-08-02T04:58:24.911334Z 01O PASS: B5: omemo skipped/logged
2026-08-02T04:58:24.911334Z 01O PASS: B5: omemo triggers no restart
2026-08-02T04:58:24.911335Z 01O PASS: B6: ratelimit skipped/logged
2026-08-02T04:58:24.911336Z 01O PASS: B6: ratelimit triggers no restart
2026-08-02T04:58:24.911336Z 01O PASS: B7: models skipped/logged
2026-08-02T04:58:24.911337Z 01O PASS: B7: models triggers no restart
2026-08-02T04:58:24.911338Z 01O PASS: B8: gibberish skipped/logged
2026-08-02T04:58:24.911338Z 01O PASS: B8: gibberish triggers no restart
2026-08-02T04:58:24.911339Z 01O PASS: B9: gateway targeted
2026-08-02T04:58:24.911340Z 01O PASS: B9: xmpp targeted
2026-08-02T04:58:24.911340Z 01O PASS: B9: tensorzero targeted
2026-08-02T04:58:24.911341Z 01O === Section C: Init-system sub-unit remediation ===
2026-08-02T04:58:24.911342Z 01O PASS: C1: critical systemd sub-unit restarted
2026-08-02T04:58:24.911343Z 01O PASS: C2: healthy sub-unit not restarted
2026-08-02T04:58:24.911343Z 01O PASS: F3: 'skipped' (not-started) unit not remediated
2026-08-02T04:58:24.911344Z 01O PASS: F3: critical sibling still remediated
2026-08-02T04:58:24.911345Z 01O PASS: C3: OpenRC sub-unit via rc-service
2026-08-02T04:58:24.911345Z 01O PASS: C4: launchd agent via launchctl
2026-08-02T04:58:24.911346Z 01O PASS: C5: first sub-unit remediated
2026-08-02T04:58:24.911347Z 01O PASS: C5: second sub-unit remediated
2026-08-02T04:58:24.911348Z 01O PASS: C6: logical degrade still remediated
2026-08-02T04:58:24.911348Z 01O PASS: C6: healthy init unit not restarted
2026-08-02T04:58:24.911349Z 01O === Section D: Per-tenant systemd user units ===
2026-08-02T04:58:24.911349Z 01O PASS: D1: tenant proxy restarts as tenant user
2026-08-02T04:58:24.911350Z 01O PASS: D1: uses systemd --user bus
2026-08-02T04:58:24.911351Z 01O PASS: D2: tenant bridge restarts as tenant user
2026-08-02T04:58:24.911352Z 01O PASS: D2: uses systemd --user bus
2026-08-02T04:58:24.911352Z 01O PASS: D3: unknown tenant falls back to system unit
2026-08-02T04:58:24.911353Z 01O PASS: D3: no per-user restart for unknown tenant
2026-08-02T04:58:24.911353Z 01O PASS: D4: missing registry → system fallback, no crash
2026-08-02T04:58:24.911354Z 01O PASS: D4: missing registry → no per-user restart
2026-08-02T04:58:24.911355Z 01O PASS: D: unit_tenant lunarwing-proxy-<t>
2026-08-02T04:58:24.911356Z 01O PASS: D: unit_tenant ironclaw-proxy-<t>
2026-08-02T04:58:24.911356Z 01O PASS: D: unit_tenant xmpp-bridge-<t>
2026-08-02T04:58:24.911357Z 01O PASS: D: unit_tenant lunarwing-<t>
2026-08-02T04:58:24.911358Z 01O PASS: D: unit_tenant base unit → no tenant
2026-08-02T04:58:24.911358Z 01O PASS: D: unit_tenant foreign unit → no tenant
2026-08-02T04:58:24.911359Z 01O === Section E: Grace period ===
2026-08-02T04:58:24.911360Z 01O PASS: E1: first observation defers (grace 1/2)
2026-08-02T04:58:24.911360Z 01O PASS: E1: no restart during grace
2026-08-02T04:58:24.911361Z 01O PASS: E2: restart fires once grace met
2026-08-02T04:58:24.911361Z 01O PASS: E3: grace=1 restarts immediately
2026-08-02T04:58:24.911362Z 01O PASS: E4: healthy-in-between resets the grace streak
2026-08-02T04:58:24.911363Z 01O PASS: E4: no restart on the reset first observation
2026-08-02T04:58:24.911363Z 01O === Section F: Backoff & retry spacing (compute_backoff) ===
2026-08-02T04:58:24.911399Z 01O PASS: F1: exp attempt=1 in [0,base]
2026-08-02T04:58:24.911399Z 01O PASS: F2: exp attempt=3 in [0,base*4]
2026-08-02T04:58:24.911400Z 01O PASS: F2: exp capped at BACKOFF_MAX
2026-08-02T04:58:24.911401Z 01O PASS: F4: linear strategy deterministic
2026-08-02T04:58:24.911402Z 01O PASS: F5: attempt>20 capped to BACKOFF_MAX (no loop blow-up)
2026-08-02T04:58:24.911402Z 01O PASS: F6: large-delay urandom jitter path stays in range
2026-08-02T04:58:24.911403Z 01O PASS: F7: non-numeric attempt → 0
2026-08-02T04:58:24.911404Z 01O PASS: F7: zero attempt → 0
2026-08-02T04:58:24.911404Z 01O PASS: F7: negative attempt → 0
2026-08-02T04:58:24.911405Z 01O PASS: F3: backoff gate defers retry
2026-08-02T04:58:24.911405Z 01O PASS: F3: no restart while gated
2026-08-02T04:58:24.911406Z 01O === Section G: Max retries & escalation ===
2026-08-02T04:58:24.911406Z 01O PASS: G1: max retries escalates
2026-08-02T04:58:24.911407Z 01O PASS: G1: escalation notification fired
2026-08-02T04:58:24.911407Z 01O PASS: G1: state marked escalated
2026-08-02T04:58:24.911408Z 01O PASS: G1: no restart once escalated
2026-08-02T04:58:24.911408Z 01O PASS: G2: escalated service is skipped
2026-08-02T04:58:24.911409Z 01O PASS: G2: no restart for escalated service
2026-08-02T04:58:24.911409Z 01O PASS: G3: escalation report written (escalated=true)
2026-08-02T04:58:24.911410Z 01O PASS: G3: escalation report names the service
2026-08-02T04:58:24.911410Z 01O PASS: G3: escalation report records the reason
2026-08-02T04:58:24.911411Z 01O PASS: G4: missing notifier warns
2026-08-02T04:58:24.911412Z 01O PASS: G4: missing notifier does not crash the run
2026-08-02T04:58:24.911412Z 01O === Section H: Flapping guard ===
2026-08-02T04:58:24.911413Z 01O PASS: H1: flapping detected
2026-08-02T04:58:24.911413Z 01O PASS: H1: flapping escalates
2026-08-02T04:58:24.911414Z 01O PASS: H1: flapping does not restart
2026-08-02T04:58:24.911415Z 01O PASS: H2: 4 < threshold is not flapping
2026-08-02T04:58:24.911415Z 01O PASS: H2: restart proceeds below threshold
2026-08-02T04:58:24.911416Z 01O PASS: H3: out-of-window restarts do not count
2026-08-02T04:58:24.911416Z 01O PASS: H3: restart proceeds when history is stale
2026-08-02T04:58:24.911417Z 01O PASS: H4: restart_history trimmed to cap (20, read from script)
2026-08-02T04:58:24.911418Z 01O === Section I: Post-restart verification ===
2026-08-02T04:58:24.911419Z 01O PASS: I1: healthy verify clears the service
2026-08-02T04:58:24.911419Z 01O PASS: I1: state empty after verified-healthy restart
2026-08-02T04:58:24.911420Z 01O PASS: I2: still-critical detected
2026-08-02T04:58:24.911420Z 01O PASS: I2: critical verify counts a retry
2026-08-02T04:58:24.911421Z 01O PASS: I3: still-degraded detected
2026-08-02T04:58:24.911422Z 01O PASS: I3: degraded verify counts a retry
2026-08-02T04:58:24.911422Z 01O PASS: I4: inconclusive verify falls back to is-active
2026-08-02T04:58:24.911423Z 01O PASS: I4: is-active(down) counts a retry
2026-08-02T04:58:24.911423Z 01O PASS: I5: --verify-health false uses is-active only
2026-08-02T04:58:24.911424Z 01O PASS: I6: missing health script → no verify log
2026-08-02T04:58:24.911425Z 01O PASS: I6: missing health script falls back to is-active
2026-08-02T04:58:24.911425Z 01O PASS: I7: non-executable health script → no verify log
2026-08-02T04:58:24.911426Z 01O PASS: I7: non-executable health script falls back to is-active
2026-08-02T04:58:24.911427Z 01O === Section J: Restart failure ===
2026-08-02T04:58:24.911428Z 01O PASS: J2: unknown manager warns
2026-08-02T04:58:24.911428Z 01O PASS: J2: restart counted as failure
2026-08-02T04:58:24.911429Z 01O PASS: J2: failed restart bumps retry
2026-08-02T04:58:24.911429Z 01O === Section K: State recovery & pruning ===
2026-08-02T04:58:24.911430Z 01O PASS: K1: report-as-truth recovery
2026-08-02T04:58:24.911431Z 01O PASS: K1: recovered state cleared
2026-08-02T04:58:24.911431Z 01O PASS: K2/K4: stale pruned, escalated preserved
2026-08-02T04:58:24.911432Z 01O PASS: K3: --prune-ttl 0 disables pruning
2026-08-02T04:58:24.911448Z 01O PASS: K5: currently-unhealthy entry preserved past TTL
2026-08-02T04:58:24.911449Z 01O PASS: K5: unrelated stale entry still pruned
2026-08-02T04:58:24.911450Z 01O === Section L: Concurrency & locking ===
2026-08-02T04:58:24.911450Z 01O PASS: L1: concurrent run detected and skipped
2026-08-02T04:58:24.911451Z 01O PASS: L1: no work while another instance holds the lock
2026-08-02T04:58:24.911452Z 01O PASS: L2: normal run completes
2026-08-02T04:58:24.911453Z 01O PASS: L2: no false lock contention
2026-08-02T04:58:24.911454Z 01O === Section O: Truncated-state recovery (6b) ===
2026-08-02T04:58:24.911455Z 01O PASS: O1: corrupt state is detected and logged
2026-08-02T04:58:24.911455Z 01O PASS: O1: corrupt file is renamed (forensic slot)
2026-08-02T04:58:24.911456Z 01O PASS: O1: run completes despite corrupt state
2026-08-02T04:58:24.911457Z 01O PASS: O1: forensic .corrupt copy preserved
2026-08-02T04:58:24.911458Z 01O PASS: O1: fresh state.json rewritten after recovery
2026-08-02T04:58:24.911459Z 01O PASS: O2: empty state quarantined (not silently reset)
2026-08-02T04:58:24.911459Z 01O PASS: O2: empty state runs cleanly after recovery
2026-08-02T04:58:24.911460Z 01O PASS: O2: empty state preserved as .corrupt
2026-08-02T04:58:24.911461Z 01O PASS: O3: single-slot rename — exactly one .corrupt file after repeated corruption
2026-08-02T04:58:24.911462Z 01O === Section M: Dry-run mode ===
2026-08-02T04:58:24.911462Z 01O PASS: M1: dry-run logs the would-be restart
2026-08-02T04:58:24.911463Z 01O PASS: M2: dry-run still tracks retry state
2026-08-02T04:58:24.911464Z 01O === Section N: CLI argument validation ===
2026-08-02T04:58:24.911464Z 01O PASS: N1: unknown flag dies
2026-08-02T04:58:24.911465Z 01O PASS: N1: unknown flag exits 1
2026-08-02T04:58:24.911466Z 01O PASS: N2: --help prints usage
2026-08-02T04:58:24.911467Z 01O PASS: N2: --help exits 0
2026-08-02T04:58:24.911467Z 01O PASS: N3: --max-retries parsed
2026-08-02T04:58:24.911468Z 01O PASS: N3: --grace-checks parsed
2026-08-02T04:58:24.911469Z 01O PASS: N3: --backoff-strategy parsed
2026-08-02T04:58:24.911469Z 01O PASS: N3: --backoff-base parsed
2026-08-02T04:58:24.911470Z 01O PASS: N3: --verify-health parsed
2026-08-02T04:58:24.911471Z 01O 
2026-08-02T04:58:24.911471Z 01O 127 passed, 0 failed
2026-08-02T04:58:24.914988Z 01O 
2026-08-02T04:58:24.914994Z 01O [1m━━━ chaos (chaos-harness.sh) ━━━[0m
2026-08-02T04:58:51.955508Z 01O === CH1: kill gateway → restart + verify → recovered ===
2026-08-02T04:58:51.955582Z 01O PASS: CH1: gateway recovers after restart
2026-08-02T04:58:51.955584Z 01O PASS: CH1: gateway is up post-recovery
2026-08-02T04:58:51.955586Z 01O PASS: CH1: exactly one restart
2026-08-02T04:58:51.955587Z 01O PASS: CH1: state cleared on recovery
2026-08-02T04:58:51.955588Z 01O PASS: CH1: no collateral restart of other services
2026-08-02T04:58:51.955590Z 01O === CH2: gateway recovers, then stays bound (no churn on next healthy tick) ===
2026-08-02T04:58:51.955592Z 01O PASS: CH2: no second restart once healthy
2026-08-02T04:58:51.955594Z 01O PASS: CH2: healthy report does no work
2026-08-02T04:58:51.955595Z 01O === CH3: kill xmpp-bridge → recovered ===
2026-08-02T04:58:51.955596Z 01O PASS: CH3: xmpp-bridge recovers
2026-08-02T04:58:51.955598Z 01O PASS: CH3: xmpp-bridge up
2026-08-02T04:58:51.955599Z 01O === CH4: kill tensorzero-gateway → recovered ===
2026-08-02T04:58:51.955600Z 01O PASS: CH4: tensorzero recovers
2026-08-02T04:58:51.955602Z 01O PASS: CH4: tensorzero up
2026-08-02T04:58:51.955603Z 01O === CH5: clickhouse crash → recovered ===
2026-08-02T04:58:51.955604Z 01O PASS: CH5: clickhouse recovers
2026-08-02T04:58:51.955605Z 01O PASS: CH5: clickhouse up
2026-08-02T04:58:51.955607Z 01O === CH6: non-recoverable fault (stuck service) → escalates after max retries ===
2026-08-02T04:58:51.955608Z 01O PASS: CH6: escalates when unrecoverable
2026-08-02T04:58:51.955610Z 01O PASS: CH6: state marked escalated
2026-08-02T04:58:51.955611Z 01O PASS: CH6: service never recovered (stuck)
2026-08-02T04:58:51.955686Z 01O PASS: CH6: restart attempts bounded at max-retries
2026-08-02T04:58:51.955687Z 01O === CH9: restart command keeps failing → escalates (no endless loop) ===
2026-08-02T04:58:51.955689Z 01O PASS: CH9: failed restart is recorded
2026-08-02T04:58:51.955691Z 01O PASS: CH9: escalates after repeated restart failures
2026-08-02T04:58:51.955692Z 01O PASS: CH9: state marked escalated
2026-08-02T04:58:51.955694Z 01O PASS: CH9: service still down
2026-08-02T04:58:51.955695Z 01O === CH12: rapid flapping → flap guard escalates, restarts capped ===
2026-08-02T04:58:51.955696Z 01O PASS: CH12: flapping detected
2026-08-02T04:58:51.955698Z 01O PASS: CH12: flapping escalates
2026-08-02T04:58:51.955699Z 01O PASS: CH12: restarts capped at flap threshold (no endless loop)
2026-08-02T04:58:51.955701Z 01O === CH13: transient blip absorbed by the grace period (no restart) ===
2026-08-02T04:58:51.955702Z 01O PASS: CH13: first blip deferred by grace
2026-08-02T04:58:51.955704Z 01O PASS: CH13: transient blip never triggers a restart
2026-08-02T04:58:51.955705Z 01O PASS: CH13: report-as-truth clears the blip
2026-08-02T04:58:51.955706Z 01O PASS: CH13: state clean after the blip
2026-08-02T04:58:51.955708Z 01O === CH10: tenant unit crash → remediated as the tenant user ===
2026-08-02T04:58:51.955709Z 01O PASS: CH10: tenant unit recovers
2026-08-02T04:58:51.955711Z 01O PASS: CH10: tenant unit up
2026-08-02T04:58:51.955712Z 01O PASS: CH10: tenant unit restarted once
2026-08-02T04:58:51.955713Z 01O PASS: CH10: tenant state cleared on recovery
2026-08-02T04:58:51.955715Z 01O === CH11: one tenant down, another healthy → blast-radius isolation ===
2026-08-02T04:58:51.955716Z 01O PASS: CH11: affected tenant recovered
2026-08-02T04:58:51.955718Z 01O PASS: CH11: affected tenant restarted
2026-08-02T04:58:51.955719Z 01O PASS: CH11: healthy tenant NOT restarted
2026-08-02T04:58:51.955721Z 01O PASS: CH11: healthy tenant left untouched
2026-08-02T04:58:51.955722Z 01O === CH14: restart 'succeeds' but unit crash-loops (SubState=auto-restart) → verify rejects (F5) ===
2026-08-02T04:58:51.955724Z 01O PASS: CH14: verify rejects crash-looping unit
2026-08-02T04:58:51.955725Z 01O PASS: CH14: a restart was attempted
2026-08-02T04:58:51.955727Z 01O PASS: CH14: crash-looping unit kept in retry state (not cleared)
2026-08-02T04:58:51.955728Z 01O 
2026-08-02T04:58:51.955729Z 01O Note: CH7 (DNS failure) is modeled by CH6's stuck-service escalation path.
2026-08-02T04:58:51.955731Z 01O Note: CH8 (disk full) requires real disk-fault injection and is out of scope
2026-08-02T04:58:51.955733Z 01O       for the mock harness — exercise it on the integration test machine.
2026-08-02T04:58:51.955734Z 01O 
2026-08-02T04:58:51.955736Z 01O 39 passed, 0 failed
2026-08-02T04:58:51.959735Z 01O 
2026-08-02T04:58:51.959741Z 01O [1m━━━ openrc (test-health-openrc.sh) ━━━[0m
2026-08-02T04:58:58.351047Z 01O PASS: S1: emits valid JSON
2026-08-02T04:58:58.351062Z 01O PASS: S1: lunarwing-proxy-acme listed exactly once (dedup, Rank 18)
2026-08-02T04:58:58.351063Z 01O PASS: S1: weechat discovered via lunarwing-* glob (Rank 18)
2026-08-02T04:58:58.351064Z 01O PASS: S1: xmpp-bridge discovered
2026-08-02T04:58:58.351065Z 01O PASS: S1: overall healthy
2026-08-02T04:58:58.351066Z 01O PASS: S1: exit 0
2026-08-02T04:58:58.351066Z 01O PASS: S1: dead pid field removed (Rank 33)
2026-08-02T04:58:58.351067Z 01O PASS: S2: ambiguous+nonzero → state stopped (Rank 10)
2026-08-02T04:58:58.351069Z 01O PASS: S2: ambiguous+nonzero → critical (Rank 10)
2026-08-02T04:58:58.351069Z 01O PASS: S2: overall critical
2026-08-02T04:58:58.351070Z 01O PASS: S2: exit 2
2026-08-02T04:58:58.351070Z 01O PASS: S3: non-keyword + exit0 → started (fallback intact)
2026-08-02T04:58:58.351071Z 01O PASS: S3: overall healthy
2026-08-02T04:58:58.351072Z 01O PASS: S4: hung probe → state timeout (Rank 5)
2026-08-02T04:58:58.351072Z 01O PASS: S4: hung probe → degraded (Rank 5)
2026-08-02T04:58:58.351073Z 01O PASS: S4: healthy unit still reported (fleet not blanked)
2026-08-02T04:58:58.351107Z 01O PASS: S4: overall degraded
2026-08-02T04:58:58.351107Z 01O PASS: S4: bounded by per-unit timeout (1s, not the 3s hang × units)
2026-08-02T04:58:58.351108Z 01O PASS: S5: missing unit → critical
2026-08-02T04:58:58.351109Z 01O PASS: S5: exit 2
2026-08-02T04:58:58.351109Z 01O PASS: S6: stopped (started tenant) → critical
2026-08-02T04:58:58.351110Z 01O PASS: S6: overall critical
2026-08-02T04:58:58.351110Z 01O PASS: S7: not-started tenant primary → skipped (F3/O5)
2026-08-02T04:58:58.351111Z 01O PASS: S7: not-started tenant sub-unit → skipped (F3/O5)
2026-08-02T04:58:58.351112Z 01O PASS: S7: weechat-adapter role maps to tenant → skipped
2026-08-02T04:58:58.351112Z 01O PASS: S7: overall stays healthy (not flipped to critical)
2026-08-02T04:58:58.351113Z 01O PASS: S7: exit 0
2026-08-02T04:58:58.351113Z 01O PASS: S8: started tenant's down unit → critical
2026-08-02T04:58:58.351114Z 01O PASS: S8: not-started tenant's down unit → skipped
2026-08-02T04:58:58.351115Z 01O PASS: S8: overall critical (driven by the started tenant)
2026-08-02T04:58:58.351115Z 01O PASS: S8: exit 2
2026-08-02T04:58:58.351116Z 01O PASS: S9: disabled tenant's down weechat → skipped
2026-08-02T04:58:58.351116Z 01O PASS: S9: disabled tenant's down weechat-adapter → skipped
2026-08-02T04:58:58.351117Z 01O PASS: S9: non-weechat down unit still critical (gate is weechat-only)
2026-08-02T04:58:58.351118Z 01O PASS: S9: overall critical (driven by proxy, not weechat)
2026-08-02T04:58:58.351118Z 01O PASS: S9: exit 2
2026-08-02T04:58:58.351119Z 01O PASS: S10: stopped weechat on disabled tenant → skipped
2026-08-02T04:58:58.351119Z 01O PASS: S10: crashed weechat-adapter on disabled tenant → skipped
2026-08-02T04:58:58.351120Z 01O PASS: S10: overall healthy — the render-units footgun is closed
2026-08-02T04:58:58.351121Z 01O PASS: S10: exit 0
2026-08-02T04:58:58.351121Z 01O PASS: S11: flag absent → down weechat still critical (backward compat)
2026-08-02T04:58:58.351122Z 01O PASS: S11: flag true → down weechat still critical
2026-08-02T04:58:58.351123Z 01O PASS: S11: unreadable registry → down weechat still critical (never masks)
2026-08-02T04:58:58.351123Z 01O PASS: S12: retired Nanocode service → skipped
2026-08-02T04:58:58.351124Z 01O PASS: S12: retired Nanocode state remains visible
2026-08-02T04:58:58.351125Z 01O PASS: S12: retired Nanocode service cannot degrade host
2026-08-02T04:58:58.351125Z 01O PASS: S12: retired Nanocode service keeps exit 0
2026-08-02T04:58:58.351126Z 01O PASS: S12: missing retired Nanocode service → skipped
2026-08-02T04:58:58.351127Z 01O PASS: S12: missing retired Nanocode service keeps exit 0
2026-08-02T04:58:58.351127Z 01O 
2026-08-02T04:58:58.351128Z 01O 49 passed, 0 failed
2026-08-02T04:58:58.353692Z 01O 
2026-08-02T04:58:58.353699Z 01O [1m━━━ systemd (test-health-systemd.sh) ━━━[0m
2026-08-02T04:59:09.750573Z 01O === Section A: not-started tenant -> skipped, overall healthy ===
2026-08-02T04:59:09.750606Z 01O PASS: A1: not-started daemon -> skipped
2026-08-02T04:59:09.750607Z 01O PASS: A2: active pg -> healthy
2026-08-02T04:59:09.750608Z 01O PASS: A3: not-started proxy -> skipped
2026-08-02T04:59:09.750608Z 01O PASS: A4: overall healthy (no false critical)
2026-08-02T04:59:09.750609Z 01O === Section B: down unit on a started tenant -> critical (F3-A) ===
2026-08-02T04:59:09.750610Z 01O PASS: B1: started daemon -> healthy
2026-08-02T04:59:09.750610Z 01O PASS: B2: down generated pg on started tenant -> critical (F3-A)
2026-08-02T04:59:09.750611Z 01O PASS: B3: overall critical
2026-08-02T04:59:09.750611Z 01O === Section C: crashed / enabled-down / unknown -> critical (fail loud) ===
2026-08-02T04:59:09.750612Z 01O PASS: C1: not-started daemon -> skipped
2026-08-02T04:59:09.750612Z 01O PASS: C2: crashed (failed) -> critical
2026-08-02T04:59:09.750613Z 01O PASS: C3: enabled+down -> critical (fail loud)
2026-08-02T04:59:09.750613Z 01O PASS: C4: unknown enable-state -> critical (fail loud)
2026-08-02T04:59:09.750614Z 01O === Section D: auto-restart -> critical; high restart count -> degraded ===
2026-08-02T04:59:09.750672Z 01O PASS: D1: auto-restart -> critical
2026-08-02T04:59:09.750673Z 01O PASS: D2: active w/ restart_count>=3 -> degraded
2026-08-02T04:59:09.750673Z 01O PASS: D3: overall critical (auto-restart dominates)
2026-08-02T04:59:09.750674Z 01O === Section E: weechat-disabled tenant -> down weechat units skipped ===
2026-08-02T04:59:09.750674Z 01O PASS: E1: disabled tenant's down weechat -> skipped
2026-08-02T04:59:09.750675Z 01O PASS: E2: disabled tenant's failed weechat-adapter -> skipped (no restart-storm)
2026-08-02T04:59:09.750675Z 01O PASS: E3: non-weechat down unit still critical (gate is weechat-only)
2026-08-02T04:59:09.750676Z 01O PASS: E4: overall critical (driven by proxy, not weechat)
2026-08-02T04:59:09.750676Z 01O === Section F: disabled weechat only-down -> healthy; gate inert when enabled ===
2026-08-02T04:59:09.750677Z 01O PASS: F1: down weechat on disabled tenant -> skipped
2026-08-02T04:59:09.750678Z 01O PASS: F2: overall healthy — the render-units footgun is closed
2026-08-02T04:59:09.750680Z 01O PASS: F3: running weechat on disabled tenant still classified normally
2026-08-02T04:59:09.750680Z 01O PASS: F4: flag true -> down weechat still critical (gate inert)
2026-08-02T04:59:09.750681Z 01O PASS: F5: flag absent -> down weechat still critical (backward compat)
2026-08-02T04:59:09.750681Z 01O PASS: F6: retired Nanocode unit -> skipped
2026-08-02T04:59:09.750682Z 01O PASS: F7: retired Nanocode unit remains explicit
2026-08-02T04:59:09.750682Z 01O PASS: F8: retired Nanocode unit cannot degrade host
2026-08-02T04:59:09.750683Z 01O === Section G: malformed registry -> degraded (ISSUE-007) ===
2026-08-02T04:59:09.750683Z 01O PASS: G1: malformed registry -> degraded
2026-08-02T04:59:09.750684Z 01O PASS: G2: malformed registry issue surfaced
2026-08-02T04:59:09.750685Z 01O === Section H: empty-but-valid registry -> degraded (ISSUE-007) ===
2026-08-02T04:59:09.750685Z 01O PASS: H1: zero-tenant registry -> degraded (explicit documented status)
2026-08-02T04:59:09.750686Z 01O PASS: H2: zero-tenant issue surfaced
2026-08-02T04:59:09.750686Z 01O === Section I: missing jq -> degraded (ISSUE-007) ===
2026-08-02T04:59:09.750687Z 01O PASS: I1: missing jq -> degraded exit code
2026-08-02T04:59:09.750687Z 01O PASS: I2: missing jq -> degraded JSON (not healthy)
2026-08-02T04:59:09.750688Z 01O PASS: I3: missing jq issue surfaced
2026-08-02T04:59:09.750688Z 01O === Section J: zero observable tenant units -> degraded (ISSUE-007) ===
2026-08-02T04:59:09.750689Z 01O PASS: J1: zero observable tenant units -> degraded
2026-08-02T04:59:09.750689Z 01O PASS: J2: observability gap issue surfaced
2026-08-02T04:59:09.750690Z 01O === Section K: failed tenant probes -> degraded (ISSUE-007) ===
2026-08-02T04:59:09.750690Z 01O PASS: K1: failed tenant probes -> degraded
2026-08-02T04:59:09.750691Z 01O PASS: K2: probe failure issue surfaced
2026-08-02T04:59:09.750691Z 01O === Section L: mixed fleet missing OS user -> degraded ===
2026-08-02T04:59:09.750692Z 01O PASS: L1: healthy tenant cannot hide missing peer user
2026-08-02T04:59:09.750692Z 01O PASS: L2: missing tenant user named
2026-08-02T04:59:09.750693Z 01O === Section M: mixed fleet selective user-bus failure -> degraded ===
2026-08-02T04:59:09.750693Z 01O PASS: M1: healthy tenant cannot hide failed peer bus
2026-08-02T04:59:09.750694Z 01O PASS: M2: failed peer tenant named
2026-08-02T04:59:09.750694Z 01O === Section N: malformed tenant user -> degraded ===
2026-08-02T04:59:09.750695Z 01O PASS: N1: missing registry user cannot be skipped
2026-08-02T04:59:09.750695Z 01O PASS: N2: invalid tenant shape surfaced
2026-08-02T04:59:09.750696Z 01O === Section O: initial daemon probe failure -> degraded ===
2026-08-02T04:59:09.750696Z 01O PASS: O1: initial peer daemon probe failure degrades
2026-08-02T04:59:09.750697Z 01O PASS: O2: initial probe failure surfaced
2026-08-02T04:59:09.750697Z 01O PASS: O3: inactive unit with unknown daemon state degrades
2026-08-02T04:59:09.750698Z 01O === Section P: tenant unit enumeration failure -> degraded ===
2026-08-02T04:59:09.750704Z 01O PASS: P1: fallback units do not hide failed enumeration
2026-08-02T04:59:09.750704Z 01O PASS: P2: enumeration failure surfaced
2026-08-02T04:59:09.750705Z 01O === Section Q: mixed fleet zero-unit tenant -> degraded ===
2026-08-02T04:59:09.750705Z 01O PASS: Q1: healthy tenant cannot hide zero-unit peer
2026-08-02T04:59:09.750706Z 01O PASS: Q2: zero-unit tenant named
2026-08-02T04:59:09.750706Z 01O === Section R: enable-state probe failure -> critical ===
2026-08-02T04:59:09.750707Z 01O PASS: R1: inconclusive enable-state for down unit fails loud
2026-08-02T04:59:09.750707Z 01O PASS: R2: enable-state probe failure surfaced
2026-08-02T04:59:09.750708Z 01O 
2026-08-02T04:59:09.750708Z 01O 52 passed, 0 failed
2026-08-02T04:59:09.755410Z 01O 
2026-08-02T04:59:09.755421Z 01O [1m━━━ summary ━━━[0m
2026-08-02T04:59:09.755424Z 01O suite            pass     fail     exit
2026-08-02T04:59:09.755660Z 01O regression         27        1        1
2026-08-02T04:59:09.755666Z 01O matrix            127        0        0
2026-08-02T04:59:09.755778Z 01O chaos              39        0        0
2026-08-02T04:59:09.755789Z 01O openrc             49        0        0
2026-08-02T04:59:09.755932Z 01O systemd            52        0        0
2026-08-02T04:59:09.755938Z 01O TOTAL             294        1
2026-08-02T04:59:09.756072Z 01O 
2026-08-02T04:59:09.756074Z 01O [31mSome suites failed.[0m
2026-08-02T04:59:10.043388Z 00O section_end:1785646750:step_script
[0K
2026-08-02T04:59:10.048249Z 00O+section_start:1785646750:cleanup_file_variables
[0K
2026-08-02T04:59:10.048276Z 00O+[0K[36;1mCleaning up project directory and file based variables[0;m[0;m
2026-08-02T04:59:10.561883Z 00O section_end:1785646750:cleanup_file_variables
[0K
2026-08-02T04:59:10.738883Z 00O [31;1mERROR: Job failed: exit code 1[0;m

