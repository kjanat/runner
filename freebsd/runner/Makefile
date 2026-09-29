# Maintainer: Kaj Kowalski <info@kajkowalski.nl>
#
# Prebuilt port: installs the `runner` + `run` binaries straight from the
# GitHub release `.tar.gz` assets (no compile), mirroring the AUR
# `runner-run-bin` package. `DISTVERSION` and the `distinfo` checksums are
# rewritten by CI (.github/workflows/freebsd-release.yml) on every release;
# the values below are only a checked-in reference snapshot.

PORTNAME=	runner
DISTVERSIONPREFIX=	v
DISTVERSION=	0.12.0
CATEGORIES=	devel
MASTER_SITES=	https://github.com/kjanat/runner/releases/download/${DISTVERSIONPREFIX}${DISTVERSION}/
PKGNAMESUFFIX=	-bin

MAINTAINER=	info@kajkowalski.nl
COMMENT=	Universal project task runner (prebuilt binary)
WWW=		https://github.com/kjanat/runner

LICENSE=	MIT
LICENSE_FILE=	${WRKSRC}/LICENSE

ONLY_FOR_ARCHS=		amd64 aarch64
ONLY_FOR_ARCHS_REASON=	prebuilt binaries are published for amd64 and aarch64 only

# Each prebuilt release tarball carries its Rust target triple in the
# basename, so pick the one matching the build host's architecture. Keep
# these in lockstep with the freebsd-* entries in npm/targets.json.
RUST_TARGET_amd64=	x86_64-unknown-freebsd
RUST_TARGET_aarch64=	aarch64-unknown-freebsd
RUST_TARGET=		${RUST_TARGET_${ARCH}}

DISTNAME=	${PORTNAME}-${DISTVERSIONFULL}-${RUST_TARGET}

# The archive is flat — runner, run, README.md and LICENSE all sit at the
# root — so there is no wrapper directory to descend into and nothing to
# compile.
NO_WRKSUBDIR=	yes
NO_BUILD=	yes

OPTIONS_DEFINE=	DOCS

PLIST_FILES=	bin/run \
		bin/runner \
		share/bash-completion/completions/run \
		share/bash-completion/completions/runner \
		share/fish/vendor_completions.d/run.fish \
		share/fish/vendor_completions.d/runner.fish \
		share/runner/runner.ps1 \
		share/zsh/site-functions/_run \
		share/zsh/site-functions/_runner \
		%%PORTDOCS%%%%DOCSDIR%%/README.md
PORTDOCS=	README.md

do-install:
	${INSTALL_PROGRAM} ${WRKSRC}/runner ${STAGEDIR}${PREFIX}/bin/runner
	${INSTALL_PROGRAM} ${WRKSRC}/run ${STAGEDIR}${PREFIX}/bin/run

# Shell completions. `runner completions <shell>` is the only generator (a
# `run completions` subcommand does not exist) and emits a single stream
# covering BOTH `runner` and `run`, with the path of the invoking `runner`
# binary baked in via `current_exe()`. Strategy:
#   1. Generate the combined stream for each shell (the freshly staged
#      binary runs natively on the build host).
#   2. Rewrite the baked ${WRKSRC} paths to ${PREFIX}/bin/{runner,run}.
#      Longer match first — `${WRKSRC}/run` is a prefix of `${WRKSRC}/runner`.
#   3. Split bash + zsh on their start-of-line boundaries so each command
#      gets its own autoload file. Fish stays as one file.
	@${MKDIR} ${WRKDIR}/_compl
	${WRKSRC}/runner completions bash >${WRKDIR}/_compl/bash.combined
	${WRKSRC}/runner completions zsh  >${WRKDIR}/_compl/zsh.combined
	${WRKSRC}/runner completions fish >${WRKDIR}/_compl/fish.combined
	${WRKSRC}/runner completions pwsh >${WRKDIR}/_compl/runner.ps1
	${REINPLACE_CMD} -e 's|${WRKSRC}/runner|${PREFIX}/bin/runner|g' \
		-e 's|${WRKSRC}/run|${PREFIX}/bin/run|g' \
		${WRKDIR}/_compl/bash.combined ${WRKDIR}/_compl/zsh.combined \
		${WRKDIR}/_compl/fish.combined ${WRKDIR}/_compl/runner.ps1
	${AWK} -v r=${WRKDIR}/_compl/runner.bash -v n=${WRKDIR}/_compl/run.bash \
		'/^_clap_complete_run\(\) \{$$/ {o=n} {print > (o?o:r)}' \
		${WRKDIR}/_compl/bash.combined
	${AWK} -v r=${WRKDIR}/_compl/_runner -v n=${WRKDIR}/_compl/_run \
		'/^#compdef run$$/ {o=n} {print > (o?o:r)}' \
		${WRKDIR}/_compl/zsh.combined
	@${MKDIR} ${STAGEDIR}${PREFIX}/share/bash-completion/completions
	${INSTALL_DATA} ${WRKDIR}/_compl/runner.bash \
		${STAGEDIR}${PREFIX}/share/bash-completion/completions/runner
	${INSTALL_DATA} ${WRKDIR}/_compl/run.bash \
		${STAGEDIR}${PREFIX}/share/bash-completion/completions/run
	@${MKDIR} ${STAGEDIR}${PREFIX}/share/zsh/site-functions
	${INSTALL_DATA} ${WRKDIR}/_compl/_runner \
		${STAGEDIR}${PREFIX}/share/zsh/site-functions/_runner
	${INSTALL_DATA} ${WRKDIR}/_compl/_run \
		${STAGEDIR}${PREFIX}/share/zsh/site-functions/_run
# Fish autoloads completion files by command basename — `runner.fish` is
# sourced on `runner<TAB>` but never on `run<TAB>`. Install the (identical)
# combined stream under both names so each command's first tab works in a
# fresh shell, without depending on session order.
	@${MKDIR} ${STAGEDIR}${PREFIX}/share/fish/vendor_completions.d
	${INSTALL_DATA} ${WRKDIR}/_compl/fish.combined \
		${STAGEDIR}${PREFIX}/share/fish/vendor_completions.d/runner.fish
	${INSTALL_DATA} ${WRKDIR}/_compl/fish.combined \
		${STAGEDIR}${PREFIX}/share/fish/vendor_completions.d/run.fish
# PowerShell has no system autoload dir — pwsh users dot-source this file
# from their `$PROFILE`:  . ${PREFIX}/share/runner/runner.ps1
	@${MKDIR} ${STAGEDIR}${PREFIX}/share/runner
	${INSTALL_DATA} ${WRKDIR}/_compl/runner.ps1 \
		${STAGEDIR}${PREFIX}/share/runner/runner.ps1

post-install-DOCS-on:
	@${MKDIR} ${STAGEDIR}${DOCSDIR}
	${INSTALL_DATA} ${WRKSRC}/README.md ${STAGEDIR}${DOCSDIR}

.include <bsd.port.mk>
