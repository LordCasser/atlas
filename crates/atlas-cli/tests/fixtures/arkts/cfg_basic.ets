// ArkTS CFG golden fixture - basic control flow patterns
function greet(name: string): string {
    const msg = "Hello " + name;
    return msg;
}

function check(x: number): number {
    if (x > 0) {
        return 1;
    } else {
        return -1;
    }
}
