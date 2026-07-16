pipeline {
    agent { label 'docker' }

    options {
        timestamps()
        disableConcurrentBuilds()
        buildDiscarder(logRotator(numToKeepStr: '30'))
    }

    environment {
        IMAGE = 'ghcr.io/swarmbot-it/swarmagent'
        REGISTRY_CREDENTIALS_ID = 'nh-jenkins-github-app-swarmbot'
    }

    stages {
        stage('Test') {
            when { changeRequest() }
            steps {
                script {
                    // fmt/clippy are enforced by the GitHub Actions CI workflow; the official
                    // rust image ships the minimal profile without rustfmt/clippy.
                    docker.image('rust:1.88-bookworm')
                          .inside("-e HOME=${env.WORKSPACE} -e CARGO_HOME=${env.WORKSPACE}/.cargo") {
                        sh 'cargo test --release --locked'
                    }
                }
            }
        }

        stage('Build image') {
            steps {
                script {
                    String shortSha = (env.GIT_COMMIT ?: 'dev').take(7)
                    env.IMAGE_TAG = "${env.BUILD_NUMBER}-${shortSha}"
                    docker.build(
                        "${env.IMAGE}:${env.IMAGE_TAG}",
                        "--label org.opencontainers.image.source=https://github.com/swarmbot-it/swarmagent" +
                        " --label org.opencontainers.image.revision=${env.GIT_COMMIT ?: ''} ."
                    )
                }
            }
        }

        // Publishing happens only from main - PR builds stop at test + image build.
        stage('Push to GHCR') {
            when { branch 'main' }
            steps {
                script {
                    docker.withRegistry('https://ghcr.io', env.REGISTRY_CREDENTIALS_ID) {
                        def image = docker.image("${env.IMAGE}:${env.IMAGE_TAG}")
                        image.push()
                        image.push('latest')
                    }
                    writeFile file: 'image.txt', text: "${env.IMAGE}:${env.IMAGE_TAG}\n"
                    archiveArtifacts artifacts: 'image.txt'
                    echo "Pushed ${env.IMAGE}:${env.IMAGE_TAG} and ${env.IMAGE}:latest"
                }
            }
        }
    }
}
