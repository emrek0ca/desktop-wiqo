(function(){
  /* WiqoConfetti.burst(): a short burst of coloured pieces over the page; removes itself. */
  var COLORS=['#ff453a','#ff9f0a','#ffd60a','#32d74b','#0a84ff','#bf5af2'];
  function burst(){
    var c=document.createElement('canvas');var w=c.width=window.innerWidth||360,h=c.height=window.innerHeight||400;
    c.style.cssText='position:fixed;left:0;top:0;width:100%;height:100%;pointer-events:none;z-index:9999';document.body.appendChild(c);
    var g=c.getContext('2d'),p=[],i;
    for(i=0;i<90;i++)p.push({x:w/2,y:h*.35,vx:(Math.random()-.5)*9,vy:-Math.random()*9-2,s:4+Math.random()*5,r:Math.random()*6,vr:(Math.random()-.5)*.4,c:COLORS[i%COLORS.length]});
    var t=0;
    (function frame(){
      g.clearRect(0,0,w,h);t++;
      p.forEach(function(q){q.vy+=.28;q.x+=q.vx;q.y+=q.vy;q.r+=q.vr;g.save();g.translate(q.x,q.y);g.rotate(q.r);g.fillStyle=q.c;g.globalAlpha=Math.max(0,1-t/110);g.fillRect(-q.s/2,-q.s/2,q.s,q.s*.6);g.restore()});
      if(t<110)requestAnimationFrame(frame);else c.remove();
    })();
  }
  window.WiqoConfetti=Object.freeze({burst:burst});
})();
