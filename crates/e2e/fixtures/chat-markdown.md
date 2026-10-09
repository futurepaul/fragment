# A useful reply

~~Old plan~~ **New plan** with *emphasis* and `inline code`.

- [x] Read the contract
  - Check **nested** items
    1. Keep order
    2. Keep HTML inert
- [ ] Ship the change

> ## A quoted heading
> Text with <https://example.com/docs> and <paul@example.com>.
>
> - A quoted list

---

Language | Result | Link
:--- | :---: | ---:
`a|b` | a\|b | https://example.com/a
Rust | ~~pending~~ | paul@example.com

```rust
let safe = "<script>alert('no')</script>";
println!("{safe}");
```

~~~text
**Code stays literal**
~~~

<img src=x onerror="window.__injected = true">
<script>window.__injected = true</script>
[bad](javascript:alert(1)) ![remote](https://example.com/track.png)
\*Escaped emphasis\* and ``a ` backtick``.
