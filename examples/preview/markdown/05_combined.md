# Combined stress test

A *paragraph* with **bold**, ***bold italic***, ~~strike~~, and `inline_code`.

## Code with surrounding text

Some prose before:

```julia
struct Point{T<:Real}
    x::T
    y::T
end

distance(a::Point, b::Point) = sqrt((a.x - b.x)^2 + (a.y - b.y)^2)
```

And some prose after the code block.

## Table of capabilities

| Feature          | Inline                    |
|------------------|---------------------------|
| Bold             | **B**                     |
| Italic           | *I*                       |
| Strike           | ~~S~~                     |
| Code             | `f(x)`                    |
| Link             | [click](https://anthropic.com) |

## Task list with math

- [x] Define **f(x)** = $f(x) = \sin x + \cos x$
- [ ] Plot for $x \in [0, 2\pi]$
- [ ] Compare with $$\int_0^{2\pi} f(x)\, dx = 0$$

## Nested quotes + lists

> Quoted intro:
>
> 1. First step
> 2. Second step with `inline code`
> 3. Third step:
>    - Sub bullet
>    - Sub bullet with **bold**
>
> Back to plain quote text.

Closing paragraph.
