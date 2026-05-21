@Library('jenkins-shared') _

pipeline {
    agent any

    options {
        timestamps()
        disableConcurrentBuilds()
        buildDiscarder(logRotator(numToKeepStr: '30'))
    }

    parameters {
        choice(name: 'ACTION', choices: ['', 'CI', 'BUILD', 'DEPLOY', 'BUILD_AND_DEPLOY'], description: 'Empty value enables automatic PR/main branch behavior.')
        choice(name: 'TARGET_ENV', choices: ['DEV', 'TST'], description: 'Standard job can deploy only to DEV or TST.')
        string(name: 'IMAGE_TAG', defaultValue: '', description: 'Existing image tag for deploy-only runs.')
        string(name: 'RELEASE_VERSION', defaultValue: '', description: 'Optional release version for image tagging. Empty uses BUILD_NUMBER.')
        booleanParam(name: 'SKIP_TESTS', defaultValue: false, description: 'Skip cargo test in CI (fmt and clippy still run).')
        booleanParam(name: 'RUN_LLM_REVIEW', defaultValue: true, description: 'Run OpenAI-compatible local LLM review for PR/MR builds.')
    }

    environment {
        INTERNAL_REGISTRY = 'registry.debian.dc4.pl'
        IMAGE_NAMESPACE = 'swarmbotty'
        REGISTRY_CREDENTIALS_ID = 'swarm-jenkins'
        SWARM_MANAGER = 'debian@debian.dc4.pl'
        SWARM_DEV_STACK = 'swarmbotty-dev'
        SWARM_TST_STACK = 'swarmbotty-tst'
    }

    stages {
        stage('CI') {
            when {
                anyOf {
                    changeRequest()
                    expression { !params.ACTION || params.ACTION == 'CI' || params.ACTION == 'BUILD_AND_DEPLOY' || params.ACTION == 'BUILD' }
                }
            }
            steps {
                rustCI()
            }
        }

        stage('Build') {
            when {
                not { changeRequest() }
                expression { (!params.ACTION && env.BRANCH_NAME == 'main') || params.ACTION == 'BUILD' || params.ACTION == 'BUILD_AND_DEPLOY' }
            }
            steps {
                rustBuild()
            }
        }

        stage('Deploy') {
            when {
                not { changeRequest() }
                expression { (!params.ACTION && env.BRANCH_NAME == 'main') || params.ACTION == 'DEPLOY' || params.ACTION == 'BUILD_AND_DEPLOY' }
            }
            steps {
                rustDeploy(params.TARGET_ENV ?: 'DEV')
            }
        }
    }
}
