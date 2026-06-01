# Building Container Images

If you do not have access to the remote registry (`registry.gitlab.com`) or prefer to compile and build the project from source, you can build the container images locally before starting the environment.

Run the following Make target from the root of the project to build the required images and start the environment:

```bash
make compose-build-up
```

For source-mounted development (using the builder image), you can use the dev target instead:

```bash
make compose-dev-up
```
