# Code

## Inline

Use `cargo build --release -p sot-frontend` to build. Backticks
around `Vec<u8>` and `Result<(), Error>` should render monospace inline.

## Fenced block — Julia

```julia
function f(x::Real)
    y = sin(x) + cos(x)
    return y
end

# Call it
result = f(π/4)
@show result
```

## Fenced block — Rust

```rust
fn main() -> anyhow::Result<()> {
    let v: Vec<u8> = vec![1, 2, 3];
    println!("v = {:?}", v);
    Ok(())
}
```

## Fenced block — plain (no language)

```
just some
indented
text
```

## Tilde-fenced

~~~bash
for f in *.jl; do
    julia --project "$f"
done
~~~
