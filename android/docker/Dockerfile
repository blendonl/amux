FROM eclipse-temurin:17-jdk-noble

ARG CMDLINE_TOOLS_BUILD=16111833
ARG PLATFORM=android-35
ARG BUILD_TOOLS=35.0.0
ARG NDK_VERSION=27.3.13750724
ARG CARGO_NDK_VERSION=4.1.2

ENV ANDROID_HOME=/opt/android-sdk \
    ANDROID_SDK_ROOT=/opt/android-sdk \
    ANDROID_NDK_VERSION=${NDK_VERSION} \
    ANDROID_NDK_HOME=/opt/android-sdk/ndk/${NDK_VERSION} \
    ANDROID_NDK_ROOT=/opt/android-sdk/ndk/${NDK_VERSION} \
    ANDROID_BUILD_TOOLS_VERSION=${BUILD_TOOLS} \
    RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo
ENV PATH=/opt/cargo/bin:${ANDROID_HOME}/cmdline-tools/latest/bin:${ANDROID_HOME}/platform-tools:${ANDROID_HOME}/build-tools/${BUILD_TOOLS}:${PATH}

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl unzip gcc libc6-dev \
 && rm -rf /var/lib/apt/lists/*

RUN curl -fsSLo /tmp/cmdline-tools.zip \
      "https://dl.google.com/android/repository/commandlinetools-linux-${CMDLINE_TOOLS_BUILD}_latest.zip" \
 && mkdir -p "${ANDROID_HOME}/cmdline-tools" \
 && unzip -q /tmp/cmdline-tools.zip -d "${ANDROID_HOME}/cmdline-tools" \
 && mv "${ANDROID_HOME}/cmdline-tools/cmdline-tools" "${ANDROID_HOME}/cmdline-tools/latest" \
 && rm /tmp/cmdline-tools.zip \
 && yes | sdkmanager --licenses > /dev/null \
 && sdkmanager --install \
      "platforms;${PLATFORM}" \
      "build-tools;${BUILD_TOOLS}" \
      "platform-tools" \
      "ndk;${NDK_VERSION}" \
 && chmod -R go=u-w "${ANDROID_HOME}" \
 && rm -rf /root/.android /tmp/*

RUN curl -fsSL https://sh.rustup.rs \
    | sh -s -- -y --no-modify-path --profile minimal --default-toolchain stable \
 && rustup target add aarch64-linux-android x86_64-linux-android \
 && cargo install --locked cargo-ndk --version "${CARGO_NDK_VERSION}" \
 && rm -rf "${CARGO_HOME}/registry" "${CARGO_HOME}/git" /tmp/*
