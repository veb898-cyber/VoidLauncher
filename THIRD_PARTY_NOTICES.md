# Third-party notices

## authlib-injector

- **Version:** 1.2.8
- **Build number:** 56
- **Project:** https://github.com/yushijinhun/authlib-injector
- **Official download API:** https://authlib-injector.yushi.moe/
- **Pinned artifact:** https://authlib-injector.yushi.moe/artifact/56/authlib-injector-1.2.8.jar
- **SHA-256:** `9c7f4343e6c82034958ffb48c14a2cb0c85928be7283103ce17da00c6d5a7b10`
- **License:** GNU Affero General Public License, version 3 or later, with the
  “AUTHLIB-INJECTOR” exception.

VoidLauncher does not bundle this JAR in the repository. It downloads the
unmodified artifact into the user's runtime cache
(`runtime/authlib-injector/1.2.8/authlib-injector.jar`) after verifying the
pinned SHA-256, and loads it only as a Java Agent for Ely.by accounts. The JAR
is never modified or placed on the Minecraft classpath.

### “AUTHLIB-INJECTOR” exception to the AGPL

> As a special exception, using this work in the following ways does not cause
> your program to be covered by the AGPL:
>
> a) Bundling the unaltered binary form of this work in your program without
> statically or dynamically linking to it; or
>
> b) Interacting with this work through the provided inter-process
> communication interface, such as the HTTP API; or
>
> c) Loading this work as a Java Agent into a Java Virtual Machine.

The complete license text, including this exception, ships inside the JAR at
`META-INF/licenses/authlib-injector.txt`.
