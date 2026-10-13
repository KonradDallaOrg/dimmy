# Loaded through CMAKE_TOOLCHAIN_FILE_aarch64_apple_darwin (.cargo/config.toml)
# by every cmake-built crate on the Mac target. It only sets these two values.
#
# llama.cpp links OpenSSL into libllama-common when it finds one, for the
# HTTPS model downloader in common/download.cpp, which Dimmy never calls
# (models come through core/src/download.rs). On a build machine with
# Homebrew it finds /opt/homebrew/opt/openssl@3 and records that absolute
# path, so the shipped app dies in dyld on every Mac without Homebrew.
# The Windows build never found an OpenSSL and has always run without it.
set(LLAMA_OPENSSL OFF CACHE BOOL "Dimmy: no OpenSSL in llama-common" FORCE)
set(CMAKE_DISABLE_FIND_PACKAGE_OpenSSL ON)
