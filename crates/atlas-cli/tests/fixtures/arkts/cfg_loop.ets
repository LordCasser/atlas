// ArkTS CFG golden fixture - loop control flow
function sum(n: number): number {
    let total: number = 0;
    for (let i = 0; i < n; i++) {
        total += i;
    }
    return total;
}

function countdown(start: number): number {
    let count: number = start;
    while (count > 0) {
        count--;
    }
    return count;
}
